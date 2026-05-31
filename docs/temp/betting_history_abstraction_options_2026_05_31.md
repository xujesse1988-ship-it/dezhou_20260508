# 下注历史抽象表示方案（6-max，betting tree 之外的选项）

工作笔记。背景：S2 实测发现显式 betting tree 在 6-max 多 bet size 下爆炸
（`{0.5,1}` > 1 亿节点 / ≥20B infoset / ≥645 GiB，见 `docs/six_max_nlhe_target.md` §S2）。
本文列出"除 betting tree 外，下注历史还能怎么抽象表示"的方案谱系，供定 6-max 抽象时取舍。

> **2026-05-31 调研评审更新**（多角度联网调研 + 对抗验证，详见
> `betting_history_abstraction_research_2026_05_31.md`）。三个改变结论的盲点已并入下文各节：
> 1. **爆炸是 width（多路）问题**，本文杠杆几乎都打 depth/encoding——补 §A4 width cap（GTO Wizard ~20×）。
> 2. **645/224 GiB 是 enumerated 上界，从没量过 reached set**（Pluribus 实 reach 62%，C5 已具备）。
> 3. **小注 EV 小且 6-max 未验证**，且其价值集中在河牌（树最轻的街）。
>
> 修正后的 bottom line：**别急着为 static A3 租 512 GB**；先跑 §待办 Phase 0 五个便宜 probe，
> 很可能推翻"必须保小注 / 必须 512GB"前提。本文若干数字/框架已据验证纠正（A3 收敛框架、
> B3 `{0.5,1}` GiB、Pluribus 内核、translation 定位），逐处标 `【验证纠正】`。

## 0. 当前做法 = 显式 betting tree（完美回忆）

infoset key = `node_id`(下注历史) + `bucket`(私牌) + `position`（InfoSetId 打包，
`src/abstraction/map/mod.rs`；`node_id` 在高位 26 bit）。

`node_id` = **预枚举抽象下注树里的位置** = 对抽象动作序列的**完美回忆**：两条不同加注序列哪怕
到达完全相同的局面（同底池、同人数、同该谁动）也是**不同 infoset**。爆炸就来自这里——re-raise
链的每条路径单独记一份。下注树由 `PublicBettingTree::build_with_abstraction` 枚举，当前**无 raise
深度上限**（`FacingRaise3Plus` 只分桶不剪树）。

下注历史不一定要这么表示。按"丢多少信息"分三类。

## A. 近乎无损的削减（不丢策略相关信息）

### A1. Raise cap（每街 re-raise 轮数上限）
直接砍掉爆炸的那条链。Pluribus/Libratus 都限制加注序列。最便宜、实现最简、能叠加 B/C。
- 实现：在树构造 / 合法动作里，本街 raise 数到 K 后只留 call/fold/all-in。
- 引擎已有 facing-raise 分桶，加个深度截断即可。

**实测（2026-05-31，vultr `df75058`，`nlhe_betting_tree_sizing` 加 `RAISE_CAP` 探针；
K = 每街 (Bet+Raise) 聚合上限、all-in 不计入且永远保留；preflop 169 / postflop 200，dense 两表 variable）**：

| 抽象 ＼ cap | K=1 | K=2 | K=3 / 无cap |
|---|---|---|---|
| `{1.0}` 1 大档 | 1.60M / 10.07 GiB | 3.83M / 23.98 GiB | 4.69M / 29.14 GiB |
| `{1.0,2.0}` 2 大档 | **4.53M / 28.27 GiB ✅** | 8.95M / 55.50 GiB ⚠ | 10.03M / 62.03 GiB ❌ |
| `{0.5,1.0}` 含小注 | 30.75M / **199.46 GiB ❌** | >100M / ≥647 GiB ❌ | >100M / ≥645 GiB ❌ |

（探针自洽：`{1.0}` cap=50 与无cap 逐字节一致；HU self-check 永不加 cap、守 240,096 节点；树规模随 K 单调。）

**结论 = A1 是"大档区的杠杆"，不是"小注的解药"**：
- **大档区有效**：`{1.0,2.0}` cap=1 = 28.27 GiB，比单档 `{1.0}` 无cap（29.14）还小、稳进 64 GiB
  → **用"每街最多 1 次加注"换来第二个 bet size**（策略表达力↑），内存不涨反降。有用档位 = K=1/2；
  K=3 起 ≈ 无cap（大注 ≤3 次加注即顶 all-in，cap 不绑）。
- **小注区无效**：`{0.5,1.0}` 最狠的 cap=1 仍 199.46 GiB（~7× 超 64 GiB），cap≥2 破亿。决定性反例：
  `{0.5,1.0}` cap=1（30.75M）比 `{1.0,2.0}` **无cap**（10.0M）还大 3×，max depth 还从 38 涨到 43
  → 小注的爆炸主因是**多路宽度 + 深筹码续局**（小注压低底池 → 6 人多路深局 → 每个 cap 后节点仍
  {fold,call,allin} 分叉 × 多街），raise cap 是**深度**杠杆、治不了**宽度**病。
- 故原"最对症、首选"只对**大档区**成立；**保留小注必须叠 B3/B4 或按 S2 弃小注，A1 单用不够**。
- **【验证纠正】A1 的 cap 是"次数"杠杆；Pluribus 真正用的是 size×raise-index**：0.5pot 只在每街
  **首次加注**合法、后续加注一律 `{1pot,allin}`（= §A3 的 raise-index 形式，不是次数 cap）。两者
  正交。Pluribus blueprint 跑这种 `{0.5,1}` 量级菜单 **full-depth 不炸**（664,845,654 序列、<0.5TB），
  靠的就是这个菜单 + lazy alloc——所以"小注不可行"可能是**平菜单 artifact**，见 §待办 (e)。

### A2. Public-state 规范化（transposition 折叠）— 字段定稿（2026-05-31，待 A2_TRANSPOSE 实测）

把"到达相同**精确局面**"的不同动作序列并成同一节点：不再用 `node_id`（动作路径，完美回忆，
`src/training/nlhe_betting_tree.rs:6-9`，路径单射测试 `:296-308`）做下注维度，改用精确局面规范化 key。
完美回忆下它们本是不同节点，合并后 = 把 betting 树折成 DAG。

**定位（别当免费午餐）**：transposition table 是**完美信息**博弈技术；不完美信息下公共局面相同 ≠ 策略
等价（两条路径带来不同对手 range，tabular blueprint 无 range 通道、infoset key 是唯一通道）。所以 A2
**对博弈动态无损、对 range 有损**——本质仍是对下注历史的 imperfect recall，只是粒度最细（不分桶）的一档。
DeepStack 敢在 public tree 推理是因为它同时携带 range + 对手 CFV，我们没有。

**key 设计原则**：A2 key = **续局博弈动态的充分统计量**。两状态 key 相等 ⟺ 续局子博弈逐节点合法动作
相同、逐叶收益相同。字段要不要进 key 只看这条。

**字段**（公共下注部分替掉 `node_id`；私有牌 `bucket_id` + `street_tag` 沿用 v2）——全是 public state
的整数函数，按 button 归一化（守 D-252 禁浮点）：

| 类别 | 字段 | 含义 / 决定续局的什么 |
|---|---|---|
| 必须显式编码 | `actor`（相对 button） | 现在该谁动 → 合法动作 + 位置 + 下一个轮到谁 |
| | 每在场座位 `committed_total` | 底池（Σ，`state.rs:331`）+ side pot 分层 / 收益（`:973,846`） |
| | 每在场座位 `committed_this_round` | 当前注级（`:750`）+ 各家欠注 + 本轮是否结束（`:722`） |
| | 每座位 `status`（Active/Folded/AllIn） | 谁在场 / 谁能动；Folded 不可派生（弃牌玩家有非零 committed）必须显式，AllIn 可派生顺手编码 |
| | `last_full_raise_size` | 最小加注额 `min_to = max + 此`（`:500`），路径依赖 |
| | `raise_option_open`（在场 bitmask） | "面对不足额 all-in 不重开加注权"规则 D-033-rev1（`:286,758`） |
| 可派生（不进 key） | stack / pot / max_committed / to_call / SPR / 下一个 actor | 都是上面字段的函数（stack = 起始码量 − committed_total，I-001） |
| 故意排除 | `last_aggressor` / 精确路径 / 街内次序 / 弃牌先后 | 只喂 showdown_order（不改按牌力分钱的收益 `:940,997`）→ 不改动态 = **A2 对 range 有损的出处** |

**A2 相对 B3 的硬优势**：`legal_actions()`（`state.rs:252-306`）是精确 committed 的**纯函数** →
**A2 key 相同 ⟹ 合法动作集相同（按构造）**，不需要 B3 的 `legal_action_set_id` 补丁、不可能撞 F17
（`info.rs:73-78`）。B3 那 100 万次"同 key 不同 legal_actions"违规正是分桶（`facing_size_bucket`/
`spr_bucket`）抹平筹码边界造成的。一句话：**A2 = 无损版的 B3，还顺手躲掉 B3 唯一那个正确性坑**。

**【验证纠正】"精确 committed 纯函数"措辞要精确（回查 `state.rs:252-306`）**：`legal_actions()` 是
`{actor 的 committed_this_round & stack、所有 active 的 max committed_this_round、`raise_option_open[idx]`、
`last_full_raise_size`、`big_blind`、`status`、terminal}` 的纯函数——**不读 `committed_total`，也不读
`last_aggressor`**。所以 A2 key **必须含派生态 `raise_option_open` + `last_full_raise_size`**（D-033 不足额
all-in 不重开加注权，`:472-475,517-521`：同 chip 向量但 short-all-in 历史不同 → 合法动作不同），不止
chip 总额；含上即 F17-free by construction 成立。**A2 改动面确实最小**：只在 `nlhe_betting_tree.rs` walk()
加一个 `HashMap<PublicStateKey, NodeId>` memo（命中即复用 NodeId、不递归），`pack_info_set_v2`/
`NlheDenseIndexer` **零改**（node_id 只是变少）；代价 = parent 指针变多父（DAG）、`path_to_root` 与
`distinct_paths_map_to_distinct_node_ids` 测试（`:296-310`）按设计反转、sizing/dense 硬编码节点数要重测。

**精化本节 informal `(各家已投筹码, 该谁动, 本街已加注数)`**：`各家已投筹码`→拆 `committed_total` +
`committed_this_round`（都要，前者管 side pot/收益、后者管当前轮）；`本街已加注数`→替换为
`last_full_raise_size` + `raise_option_open`（NLHE 无 3-bet cap，加注次数本身不 gate 动作）。⚠ 但 A2
**叠 §A1 raise-cap** 时，"本街加注次数（capped）"要重新进 key（cap 到顶反过来砍 sized raise）。

**与 §B3 非嵌套**（不是谁包含谁）：A2 几何精确但**丢 aggressor**；B3 几何分桶却**保 aggressor 补 range**。
谁的 range-skew 小看局面。A2 确定占优的只有"动态无损 + legal_actions 天然一致"——纠正"A2 严格最小 skew"
的过度说法。

**【验证纠正】A2 与 B3 本是一家，应统一设计**：Lanctot well-formed 条件 (iii) `X₋ᵢ(z)=X₋ᵢ(φ(z))`
（合并的两序列对手动作必须一致）经回查 ICML'12 原文成立——**任何 transposition 要 sound，必须补能区分
对手动作的字段**。所以推荐设计 = **A2 exact-public-state key（骨架，F17-free）+ last_aggressor + per-seat
`capped` 位（§B3 字段表新增）**，取代"A2 vs B3 二选一"。注意补 last_aggressor 是**必要不充分**（完整
Theorem-1 还要 (i)(ii)(iv) 的 future 同构 + 收益/chance 成比例），故仍须实测裁定。

**例**（HU turn 起手）：P = preflop SB 加注到 3 + flop 下 3 被跟；Q = preflop SB **limp 到 1** + flop 下 5
被跟。两边 turn 起手 committed_total=(6,6)、actor=BB、committed_this_round=(0,0)、双方 Active、
`last_full_raise_size`=BB（新街重置 `:701`）、`raise_option_open`=(true,true) 全字段相等 → **同 A2 key 合并**
（赢）；但 SB range 一个是加注、一个是 limp，被强制共用策略（丢 = "忘了底池怎么变大的"，且 A2 不带
aggressor 连"是否侵略过"都分不出）。

**能省多少 = 待实测**：两股相反的力——pot-relative 乘法可交换（pot×f₁×f₂=pot×f₂×f₁）+ all-in 漏斗（利于
合并）vs **固定行动顺序**（杀死换手顺序 transposition）+ 小注深多路"精确几何本就多"（不利，§A3 已证小注
病根在底池几何）。prior：无损压缩真实但**明显小于** B3 的 39–515×（B3 头条压缩来自有损分桶不是
transposition），A2 单独大概率喂不进 64 GiB 的 `{0.5,1.0}`。但它量出"免费部分"多大 = B3 分桶到底多扔
多少 range（= 多大风险），按"正确性优先"上 B3 前应先拿这个数。

**改动面**（比 §B3 小）：`pack_info_set_v2` 换 key 源（`node_id` → `betting_state_key(&GameState)` 整数）；
dense indexer（`nlhe_dense.rs:9-13`）从按 `node_id` prefix-sum 改为走树收集 distinct A2 key 分配下标，或
直接走 HashMap 后端（C5，key 空间稀疏）；**省掉** B3 的 `legal_action_set_id`。imperfect recall 收敛无
保证 → 仍须 exploitability/LBR 对 `{1.0}` perfect-recall baseline 实测裁定，差 ≥10× 停下追"摘了什么
value-critical 信息"（典型嫌疑：丢 aggressor → range 不分 → 加回 `last_aggressor` 退化成 §B3）。

### A3. first-bet-small（小注只当开池领打，re-raise 一律大注）— 需 ~512 GB 大机，可行
想保住 postflop 领打的尺寸选择（0.5 vs 1pot，策略上值钱），又躲开小注的爆炸：preflop `{1.0}`、
postflop `{0.5,1.0}` 但**只允许首次进攻（开池 `Bet`）打 0.5 或 1pot，后续 `Raise` 一律 1pot（禁 `Raise{0.5}`）**。
用 `AbstractAction` 的 `Bet`(首次)/`Raise`(后续) 天然区分实现。

**实测（2026-05-31，vultr `8b04aee`，`FIRST_SMALL=1` 探针 commit `8b04aee`；preflop 169 / postflop 200）**：

| 配置 | 决策节点 | dense 两表 | max depth |
|---|---|---|---|
| first-bet-small 无cap | **35.13M** | **224.58 GiB** | 43 |
| + RAISE_CAP=2 | 32.46M | 208.16 GiB | 43 |

放进谱系：比 `{1.0,2.0}`（10.0M/62 GiB）大 **3.6×**，比 `{0.5,1.0}` cap=1（30.75M/199 GiB）略大。
**超出 64/96 GiB 单机预算，但落进 ~512 GB 大内存机富余**（RSS ≈ 230 GiB；512 GB = 476.8 GiB 留 ~250 GiB 余量；
256 GB = 238 GiB 偏紧不建议）。raise cap 叠加也省不下来（cap=2 仍 208 GiB，cap 不绑这种深多路续局）。

**为什么反而更大 = 小注的病根在底池几何，不在 re-raise**：砍掉 0.5 re-raise（相对全程 `{0.5,1.0}` 把
≥645 砍到 224 GiB，确实砍了 ~3×），但留下的 0.5 **开池**仍制造"小底池 → 6 人深筹码多路续局"：开池打 0.5
压低底池 → 后续 1pot re-raise 按小底池算、更小 → all-in 前多塞一轮 → 树更深（depth 43 vs `{1.0}` 的 38），
且河牌一条街就占全树 3/4（per-street River=26.2M）。**只要 0.5 存在（哪怕只当领打）就触发这套几何，结构性
限制 re-raise 删不掉它。** 印证 S2「档数不是病根、小注才是」。

**取舍（不否决，是换预算）**：
- 内存：要 ~512 GB 大机（对齐 `feedback_high_perf_host_on_demand` 报预算起机），非 64 GiB c6a.8xlarge。
- 真瓶颈是训练时长：infoset 7.02B ≈ `{1.0}`（933.9M）的 **7.5×**，收敛 update 数随之上去，wall 比
  `{1.0}`/`{1.0,2.0}` 长一截——内存放开后这才是要算的账。
- 回报：**无损**保住 postflop 领打 0.5/1pot 尺寸选择（全大档抽象给不了，策略上值钱）。
- 对照 B 类：B3/B4 在小机上也能留小注尺寸（折叠成 SPR 桶），但**有损** + 改 InfoSetId 语义；first-bet-small
  纯靠堆内存、**无损保留路径**。二选一，或先用 first-bet-small 摸质量上限，再决定要不要省成 B 类。

**【验证纠正】A3 取舍的三处修正**：
- **"无损保住收敛保证"对 B3 的优势被夸大**：6-max 一般和**本来就没有** Nash 保证（A3 无 Nash 优势）。
  A3 真正比 B3 多的是 **no-regret / regret-bound by construction**（perfect recall 保留；B3 只在分桶恰好
  CRSWF 时保留，未验证，F17 那 ~100 万违例即其脆弱证据）——是 **no-regret/correctness 优势，不是 Nash**。
  正确性红线下不能当 illusory，但也别按"保 Nash"卖。
- **224 GiB = Pluribus 那张表的机器级（<0.5TB）**，正是 Pluribus 实际用的机器，不是 64 GiB；A3 本质≈
  "0.5 仅 raise-index 0" = Pluribus 菜单的一种。
- **可省的对偶杠杆**：小注 EV **集中在河牌**（终局、无再加注爆炸），故 **"0.5 只在 turn/river 开池"**
  能用零头树成本拿走大部分 EV——比"全街 0.5 开池"（现 224 GiB）更便宜。且 224 GiB 是 **enumerated 上界**，
  C5 lazy 下真正绑内存的是 **reached set**（Pluribus 62%），从没量过，见 §待办 (f)。

### A4. Width cap（限制同时在场人数）— 新增（2026-05-31 调研，待 WIDTH_CAP 实测）

**【填补盲点】爆炸的病根是 width（多路），但 A1/A2/A3 全是 depth 或单节点 encoding 杠杆，B3 是
encoding——没有一个限制"同时在场人数"。** A4 直接砍宽度：preflop 后只允许 ≤N 人进入后续街（N=2/3），
用确定性"closing-action / 投入最多者优先"规则决定谁留下（如 BTN 已 call 时 SB 只能 squeeze 或 fold）。

- **业界先例**：GTO Wizard AI 对 6-max preflop 树用"最多 3 人见 flop"砍 **~20×**。这是真正的 width 杠杆
  （攻 multiway 分支因子），与 A1（depth）/B3（encoding）正交、可叠加。
- **实现**：作为 position-asymmetric 的 `legal_actions()` 限制，**必须统一施加**（两个 width-cap 生效的态
  仍共用合法动作集），否则重蹈 F17。
- **取舍**：改了游戏（强制部分玩家出局）→ 对完整 6-max 博弈有损，但对"实战里 4+ 人打到河"这种罕见且
  策略价值低的线损失小；直击 A1 治不了的"6 人各自再加注"组合。
- **待实测**：`nlhe_betting_tree_sizing` 加 `WIDTH_CAP` probe：枚举 `{0.5,1}` 但剪掉 preflop 后 >N 人在场的
  节点，量 node/infoset/reached（§待办 (e)）。

## B. 有损削减（用摘要替代完整序列 = 把信息抽象用在下注历史上）

本质：把"下注历史"当成另一个要做信息抽象的维度，跟对私牌做 bucket 同理。

### B3. 紧凑 betting-state 摘要（feature tuple）— 设计草案（2026-05-31，待 sizing 工具实测）

不存完整路径，只存一个有界小元组当 infoset 的下注历史维度。大量不同动作序列 → 同一 key = 对下注历史做
imperfect recall（§B4 的具体编码）。**这恰好让 `{0.5,1}` 的爆炸消失**：不管底池怎么涨到这么大、经过几轮
re-raise，只要落进同一 `(SPR 桶, 面对尺寸桶, 本街加注结构, ...)` 就是同一 infoset，key 空间由字段笛卡尔积
封顶，**几乎与 bet-size 菜单无关**。

**理论依据 / 业界实践（2026-05-31 调研，见文末参考）**

- **续局价值的充分统计量 ≈ 底池 / SPR，不是路径**——商用 solver 的标准做法。PioSOLVER/GTO+ 类解 postflop
  时，preflop 一长串"谁加注谁跟"被压成一个**起手底池 + 有效筹码**，不记是谁怎么把池子打大的（GTO Wizard:
  "different postflop solutions … based on initial pot sizes … *without needing to consider who specifically
  raised and called*"）。DeepStack 的价值网络输入是 `(双方 range, 底池, 公共牌)`——同样把下注历史摘成底池一个标量。
  **但有个关键限定**：DeepStack 敢只喂底池，是因为它**同时喂 range**——range 已把下注历史的后果编码了（一路开火
  的人 range 自动是强的）。我们是 **tabular blueprint，无 range 输入，infoset key 是唯一通道**，所以对我们 SPR 只是
  **博弈动态**（能下什么注、赌注多大）的充分统计量、**不是 range 分布**的；range 不对称必须靠 `last_aggressor` 进 key 撑
  （别把 SPR 当成 range 的充分统计量——这是初版措辞的过度声称，已纠正）。
- **理论上有界**。Kroer & Sandholm EC'16（*Imperfect-Recall Abstractions with Bounds*，把 Lanctot et al. 2012
  的 skew well-formed games 推广到 CRSWF）给出把两个 infoset 并进同一抽象 infoset 的**解质量上界** = reward
  error（合并叶子收益差，按到达概率加权）+ chance error（自然到达概率差）。落到下注历史上 = **两条到达相同
  `(街, 底池/SPR, 该谁动, 本街加注结构)` 的序列续局价值几乎相等 → reward error 小 → 合并安全**。这正是拿 SPR 桶
  当 key 的依据。
- **诚实的反面：顶级 solver 并不摘下注历史**。Pluribus/Libratus 对下注历史用**完美回忆**（supp: "two infosets
  are bucketed together iff they share the *same action-abstraction sequence* and the same info bucket"——只摘
  **牌**不摘下注）；靠 ① 每街限档（首街最细、后街粗，1–14 档）+ ② **lazy 分配**（action sequence 第一次被访问
  才分配 regret，省 >2×，= 我们的 C5）+ ③ 负 regret 剪枝，把 6.65 亿动作序列做下来。文献里 imperfect recall
  的成熟战绩（Johanson et al. AAMAS'13：同内存下 imperfect recall 在 exploitability + 单挑都**胜过** perfect
  recall）几乎都是摘**牌**（忘前街桶、把桶预算压到当前街），摘**下注历史**远没那么多先例。**⇒ B3 有理论撑，但比
  "摘牌"激进、业界验证少；质量必须实测，不能假设收敛。**

**字段设计（定稿 2026-05-31，6-max；HU 是其子集）**

完整 infoset key 两部分：私牌 + 街沿用当前 v2，下注摘要 6 字段替掉 `node_id` 完整路径。全部是 public state 的整数
函数（双方可见、与私牌无关；禁浮点 D-252，桶边界用定点比较）：

| 字段 | 编码 / 取值 | bit(6-max) | 跨街? | 作用 |
|---|---|---|---|---|
| `bucket_id` | preflop 169 无损 / postflop 500 k-means 桶 | 24 | — | 私牌强度 |
| `street_tag` | Preflop/Flop/Turn/River | 3 | — | 街 |
| `actor_position` | 相对 button 0..5 / HU 0..1 | 3 | 当前 | 该谁动 + 位置 |
| `live_players` | **相对 button 在场 bitmask**，1 bit/座位（`Active∪AllIn`=1，`Folded`=0） | 6 | 累积（fold 永久） | 6-max 多路结构（记住"谁在场+在哪"，非仅人数） |
| `raises_this_street` | `{0,1,2,≥3}`，cap=K（= A1 那个量，当**特征**不剪树） | 2 | 当前（每街 reset） | ①**决定合法动作** + 本街激进度 |
| `facing_size_bucket` | `{无活注, ≤0.5p, ~1p, ≥2p, ~all-in}` | 3 | 当前 | ①**决定合法动作** + pot odds |
| `spr_bucket` | log 间距 ~12 桶（有效**剩余**筹码 / 当前底池） | 4 | 累积 | ②续局**物理量**（深度） |
| `last_aggressor` | 2 槽 `{preflop_aggressor, postflop_line_aggressor}`，各 `{无} ∪ 相对 button 座位` | 6 | 累积 | ②续局 **range/initiative** |
| `capped`【新增】 | per-seat 1 bit：该座位是否**只跟过加注**（capped）vs **(re)raise 过**（uncapped） | 6 | 累积 | ②续局 **range 形态**（capped vs polar） |

**设计决定（本轮敲定）**：

- `live_players` 用 **bitmask 不用计数**——6-max 里 button 在 vs UTG 在剩余 range/位置完全不同；硬约束
  `actor_position ⊂ live_players` 砍掉大量非法组合。不另花 bit 区分 Active/AllIn（深度后果已被 `spr_bucket` 吃掉）。
- `last_aggressor` 从"本街 1 个"扩成 **2 槽（preflop + postflop 线），相对 button**——过去街 aggressor 可能已弃牌，
  相对 actor 的"前/后"会坏，相对 button 是固定坐标系才记得住。**删掉"我是否 aggressor"**（= `last_aggressor==我` 可
  导出）和单独 initiative bit（已被 `preflop_aggressor` 槽吸收）；只记 2 槽（PFR + postflop 线）而非满 4 街，避免滑向完美回忆。
- **【验证新增】`capped` per-seat 位**：调研一致指出 range 形态（capped vs uncapped/polar）由"谁**跟了**
  加注 vs 谁**加注**"决定，**不是 `last_aggressor`（谁最后开火）**——cold-caller in position 经常 range 占优，
  aggressor 是 range 优势的 noisy proxy。`last_aggressor` 抓 initiative、抓不到 cappedness，正是 limp-vs-raise /
  cold-call-vs-3bet 反例的命门。`capped` 是纯 public-state 函数、~6 bit，是恢复被 `spr_bucket` 糊掉的 range 的
  **最小且最对**的字段。range 损失集中在 turn/river → 该位（及其它 recall 位）优先服务后街。
- **两个作用**：① `raises_this_street` + `facing_size_bucket`（含"无活注"）**必须**完整决定合法动作集，否则重蹈
  F17（同 key 但合法动作不同 → regret 矩阵错位）；② `spr_bucket`（物理量）+ `live_players` + `last_aggressor`
  （range/initiative）逼近续局价值。
- **跨街信息只靠 `spr_bucket` / `live_players` / `last_aggressor` 三个累积字段扛**；其余只看当前街。**仍丢**：精确路径、
  桶外精确尺寸、街内逐手次序、fold 先后——这是 imperfect recall 的有损部分。

**位预算**：下注摘要 = `actor_position`3 + `live_players`6 + `raises_this_street`2 + `facing_size_bucket`3 +
`spr_bucket`4 + `last_aggressor`6 = **24 bit**；**加 `capped` 6 + `legal_action_set_id` ~3 → ~33 bit**。可用位 =
v2 弃用的 `position(4)+stack(4)+betting_state(3)` + reserved 26 = 37 bit，仍**富余**；全 key（含 `bucket_id` 24
+ `street_tag` 3）≤ 60 bit < 64。

**key 空间 / infoset 实测（2026-05-31，vultr `9ddf6b3`，`nlhe_betting_tree_sizing` 加 `B3_SUMMARY` 探针，
postflop 500）**

把估算变成了数字。探针在现成 walk 里逐决策节点算上表的摘要 key，数 distinct key / B3 infoset / B3 dense 两表，
并断言"同 key 节点 `legal_actions` 一致"。两种 key：**纯字段**（暴露不变量）vs **pin 动作集**
（`B3_PIN_ACTIONS=1`，把合法动作集签名折进 key 高位 = 下方「修法」）。node_id 基线同跑（postflop 500，故比 §S2 的
postflop 200 大 ~2.5×）：

| raise 集 | node_id infoset / 两表 | B3 distinct key（纯/pin） | B3 infoset / 两表（pin） | 压缩 | 纯字段不变量 |
|---|---|---|---|---|---|
| `{1.0}` | 2307.5M / 71.98 GiB | 116,908 / 120,643 | 58.71M / **2.28 GiB** | 39× | ✗ 19,457 |
| `{1.0,2.0}` | 4939.4M / 153.16 GiB | 145,502 / 150,370 | 72.86M / **3.04 GiB** | 68× | ✗ 28,032 |
| `{0.5,1.0}` † | ≥49,654M / ≥1603 GiB | 172,352 / ≥197,432 | ≥96.50M / **≥4.86 GiB** | 515× | ✗ 1,016,860 |
| HU self-check | 119.7M / 4.62 GiB | 1,616 | 0.79M / 0.05 GiB | 152× | ✗ 8,624 |

† `{0.5,1.0}` node 枚举撞 `NODE_CAP=100M`（树 >1 亿节点），故该行 node_id 与 B3 都是**下界**。
**【验证纠正】"应已近饱和"是错的**：cap sweep（pin）实测 5M→27,455 / 20M→73,259 / 50M→104,377 /
100M→197,432 key，**仍每翻倍 ×~1.9（未饱和）**；真值可能 high-10⁵~10⁶（pure-field 上界 2²⁴≈16.7M）。
故"172K ~10⁵"**低估**，"~4.86 GiB"是**未饱和下界、不可用于定机器**。但**"进 64 GiB"定性稳**（即便真值
~2M key：~2M×500 桶×~3.4 动作×8B×2 表 ≈ 48 GiB < 64）。需用更大 NODE_CAP / key-only walk 重测真值，见 §待办 (g)。

**两条结论（坐实了 §B3 的核心论点）**：

1. **B3 的命门成立——key 空间有界且几乎与 bet-size 菜单无关**。distinct key 三档全是 ~10⁵（117K/146K/172K），而
   node_id 树从 469 万节点炸到 ≥1 亿（≥21×）：`{0.5,1.0}` 的 key 只比 `{1.0}` 多 ~1.5×，**不是 ~21×**。那个原本
   ≥1603 GiB（≥645 见 §S2 postflop 200）单机无解的 `{0.5,1.0}`，**B3 下进 64 GiB 小机**（~4.9 GiB 是**未饱和
   下界**，见 † 与 §待办 (g)；真值更大但定性进 64 GiB 稳）——B3 把小注的组合爆炸直接化解。压缩比 39–515×
   （`{0.5,1.0}` 那档因未饱和是**下界**），档越多/越小压得越狠。这就是 B3 相对 A3（首选但要 ~512 GB 大机）的根本好处。
2. **纯字段 key 不足以决定合法动作集（不变量被破，必须修）**。三档都出现"同 key 不同 `legal_actions`"（`{1.0}`
   1.9 万次 / `{0.5,1.0}` 100 万次）。病根：`facing_size_bucket`/`spr_bucket` 把"sized raise 还构造得出吗"这个精确
   筹码边界抹平了，落同桶的两个节点一个能 raise、一个只能 call/fold/allin → 合法动作集不同 → 重蹈 F17（regret 槽
   错位）。**生产修法 = key 必须含合法动作集信息**（`B3_PIN_ACTIONS` 把 `action_sig` 折进 key）。代价**几乎为零**：
   pin 后 key 只多 ~3%（`{1.0}` 120,643 vs 116,908；`{0.5,1.0}` +15%），不变量按构造成立——说明动作集本就几乎被字段
   决定，违规只在少量边界 key。**字段表（上）须再加一项 `legal_action_set_id`**（由 `state.legal_actions()` 派生的
   几 bit），否则 dense stride 与 regret 槽无法对齐。

**与 InfoSetId 打包的改动面**

好消息：64-bit layout **本来就为此留了位**。当前 v2 packer（`nlhe.rs:pack_info_set_v2`）把 26 bit reserved 塞
node_id，而把 stage-2 设计的 `position_bucket`(4)/`stack_bucket`(4)/`betting_state`(3) 三个字段**置 0 弃用**
（见 `src/abstraction/map/mod.rs` layout）。B3 = 把这套字段**复活并扩充**（`betting_state` 5 状态 → 上表多字段
摘要，借 reserved 26 bit 装 spr/facing/aggressor/live_players），位预算绰绰有余（需 ~20 bit < 37 bit 可用）。改动点：

1. `pack_info_set_v2` → `pack_info_set_b3(bucket, summary_key, street)`：`summary_key` **由 `GameState` 算**，不再由
   tree `node_id` 给。这是核心语义变更：InfoSetId 从"树路径的单射"变成"public state 的多对一摘要"。
2. 新增 `betting_summary_key(state: &GameState) -> u32`（纯整数，守 `map/mod.rs` 的 D-252/D-273 禁浮点；SPR 桶边界
   用定点比较，不进 `f64`）。
3. **关键不变量（不可破）**：`summary_key` **必须**完整决定该节点合法动作集，否则重蹈 F17（`Open` 与
   `FacingBetNoRaise` 同 key 但合法动作不同 → CFR regret 矩阵错位）。即 `raises_this_street` + `facing_size_bucket`
   （含"无活注"）要够还原 {可 check? 可 bet? 必须 fold/call/raise? raise 是否被 cap 封}。合并前先证——可在 sizing
   工具里断言"同 key 的所有 tree 节点 `legal_actions` 一致"。
4. dense indexer（`nlhe_dense.rs`）：现按 `node_id` 一节点一 meta；B3 改按 **distinct `summary_key` 一 key 一 meta**。
   建表从"走树"变成"走树 + 收集 distinct key"（每 key 的 `bucket_count`/`action_count` 取该 key 任一代表节点，前提
   不变量 3 成立）。或：key 空间稀疏（~3 万）时直接用 HashMap 后端（C5），dense 预分配意义变小。
5. `current_node_id` 仍保留——树还要用来枚举合法抽象动作 + 推进 state；只是**不再进 infoset key**。AIVAT 值表
   （`aivat_value.rs` 按 `(node_id, bucket)`）同步改按 `(summary_key, bucket)`。

**风险 / 取舍**

- **有损 + 弱收敛**：imperfect recall 让 CFR 完美回忆收敛保证失效（2 人零和也不保证 Nash）。按项目"正确性优先"
  红线，B3 对错只能由 exploitability/LBR/对 Slumbot 实测裁定，**不能假设收敛**；若质量比 `{1.0}` perfect-recall
  baseline 差 ≥10× 要停下追"摘掉了什么 value-critical 信息"（典型嫌疑：忘了底池被动跟大 vs 被加注打大 → `last_aggressor`
  不够细 → 加细）。
- **改 InfoSetId 语义**：`node_id`（精确路径，单射）→ 计算摘要 key（多对一），波及 checkpoint 兼容、AIVAT 重建、
  所有按 node_id 索引的诊断。
- **6-max 特有**：`live_players` 编码（计数 vs bitmask）是质量/大小的主旋钮；HU 阶段先验证再上 6-max。

### B4. Imperfect recall（不完美回忆）
"忘掉"前几街动作细节，只保留当前街摘要 + 粗粒度底池/SPR。**大规模求解器扩状态空间的标准手段**
（Libratus/Pluribus 的牌面抽象就是 imperfect recall，下注历史同理）。B3 是它的一种具体编码。

## C. 换表示方式（不再枚举树）

### C5. Lazy / hash 生成 infoset（无预枚举树）
我们的 **HashMap 后端本来就这样**：不预建树，采样访问到才 materialize。
- 不减少 infoset **数量**，但省掉上前枚举，且能直接吃 A/B 的摘要 key（摘要空间稀疏无妨）。
- dense 表才必须预枚举有界 key 空间——而 B 的摘要恰好给出一个**有界且小得多**的 key 空间。

### C6. 连续 / 神经编码
把下注历史编码成特征向量喂价值/策略网络（ReBeL / DeepStack / AlphaHoldem），彻底绕开 tabular 枚举。
= 之前 parked 的神经路线（与当前纯 Rust tabular-CFR 基建脱节）。

## 诚实的权衡

- **B 类有损**：bot 会"忘记"区分（如忘了底池是激进打大还是被动跟大）。摘要选不好掉策略质量；
  **CFR 在 imperfect recall 下收敛保证变弱**（2 人零和都不保证收敛 Nash，靠经验 + 好抽象）。
  艺术在于摘要保住策略相关信息。
- B 类**改 InfoSetId 语义**：`node_id`（精确路径，一对一）→ 计算出的摘要 key（多对一）。
- **A 类（raise cap / 状态规范化 / first-bet-small）风险最低、能与 B/C 叠加**。其中 A1/A2/A3 **对单节点信息
  无损**（A2 仅丢 range，对动态无损）；**A4 width cap 改了游戏**（强制部分玩家出局，对完整 6-max 博弈有损），
  但实战价值低、直击 A1 治不动的 width。对小注：单机 64 GiB 预算下 raise cap 救不了（§A1，仍 199 GiB）；
  first-bet-small 保住小注尺寸但要 **~512 GB 大机**（§A3，224 GiB enumerated 上界）——可行但贵，且其"无损保
  收敛"在 6-max 是 no-regret/soundness 优势而非 Nash（§A3【验证纠正】）。**先 Phase 0、别先买机器。**
- **B 类弱收敛的实际权重**：6-max 一般和**本就无 Nash 保证**，B3 的 imperfect-recall 收敛代价**大部分已由
  转 6-max 付掉**；剩下的边际代价（no-regret/soundness）须靠 HU 零和管线 + 实测对战裁定，不能假设。

## 对我们最对症的组合

爆炸 = **re-raise 深链（A1 可砍 depth）+ 小注底池几何导致的多路深筹码续局（A1/A3 砍不动深度）+ 多路
**width**（6 人各自再加注，A4 才砍得到，见 §A4）**。

**【验证纠正】决策框架：先修测量再选方案（cheapest-first），别先为 static A3 租 512 GB。** 原"当前倾向
先走 (i)"被三点推翻：A3 的"无损保 Nash"在 6-max 是 no-regret/soundness 优势而非 Nash（§A3）、224 GiB 是
enumerated 上界从没量 reached（§A3/§盲点 2）、小注 EV 小且集中在河（§盲点 3）。正确顺序：

- **Phase 0（几天，便宜，先做）**：跑 §待办 (e)(f)(g) 五个 probe——raise-index/街菜单 + A4 width cap +
  reached-set + B3 真 distinct-key。很可能直接把"必须 512GB"降到 64–96 GiB。
- **Phase 1（据 Phase 0 选路）**：
  - 若小注子集能压进 64–96 GiB → 走**无损路线**（coarse 菜单 + A2 transposition memo 无损挤压），保
    no-regret/soundness，躲 B3 无业界先例的"摘下注历史"。
  - 若无论如何无损塞不下、且质量 probe 证明小注值 → 上**重设计 B3** = A2 exact-key + last_aggressor +
    `capped` 位 + `legal_action_set_id` pin，**先在 HU 零和管线验**（exploitability/LBR 在那才有牙齿），再上 6-max。
- **Phase 2（不论选哪条都做）**：`map_off_tree` 升 PHM；VR-MCCFR baselines 治训练 wall；A-loss 断言 +
  range-skew(KL/EMD) 实测把"有损"量成数字。

仍想要 postflop 小注尺寸的三条老路（现按上面框架重排优先级）：

- **(iii) 直接弃小注**：少量大档 + A1（`{1.0,2.0}` cap=1 已进 64 GiB），最省事——**Phase 0 之外的稳妥兜底**。
- **(ii) 小机 + B3 摘要**（重设计版，见 §B3 + §A2 统一段）：key 空间有界、爆炸消失且与档数几乎无关；代价
  = **有损 + 弱收敛**（须实测裁定，先 HU 验）+ 改 InfoSetId 语义。理论有界（Kroer-Sandholm CRSWF）但摘下注
  历史业界少见，比"摘牌"激进；是把 A3 省成小机的路子。
- **(i) ~512 GB 大机 + first-bet-small（§A3）**：无损保留完整路径，224 GiB 两表（enumerated 上界）落进 512 GB。
  **降级为"Phase 0 证明小注确实塞不下小机、且确实值"之后才考虑**，不再是默认首选。

## 待办 / 下一步候选

- (a) ✅ **已做**（2026-05-31，见 §A1 实测，`RAISE_CAP` 探针 commit `df75058`）：raise cap=K 对
  `{0.5,1}` **无效**（cap=1 仍 199 GiB，cap≥2 破亿），对 `{1.0,2.0}` **有效**（cap=1 = 28.27 GiB 进
  64 GiB）。结论：A1 = 大档区杠杆，非小注解药。
- (b) ✅ **已草拟**（2026-05-31，见 §B3 设计草案）：保留字段（`actor_position` / `live_players` /
  `raises_this_street` / `facing_size_bucket` / `spr_bucket` / `last_aggressor`）+ key 空间量级估算（~3–4 万
  betting key、~1600 万 infoset、dense 两表 <1 GiB，待实测）+ 与 InfoSetId 打包的改动面（复活 v2 弃用的
  `position/stack/betting_state` 字段 + reserved 26 bit；`pack_info_set_v2`→`_b3`；dense indexer 改按 distinct
  key；关键不变量 = key 必须决定合法动作集，否则重蹈 F17）。
  （A1 既已证救不动小注，(b) 的有损摘要从"可选"升为"想保留小注就必需"；若走"弃小注 + 大档 + A1"
  路线则可暂不做 (b)。）
- (c) ✅ **已做**（2026-05-31，见 §B3「key 空间 / infoset 实测」，`B3_SUMMARY` 探针 commit `9ddf6b3`）：
  distinct key 三档全 ~10⁵、与档数几乎无关，`{0.5,1.0}` ≥1603 GiB → ~4.9 GiB 进小机（命门成立）；但纯字段 key
  不变量被破（同 key 不同 `legal_actions`），修法 = key 加 `legal_action_set_id`（`B3_PIN_ACTIONS` 验证，pin 后
  规模只 +3~15%、不变量按构造成立）。
- (d) **下一步**：把 `legal_action_set_id` 正式列入字段表（已在 §B3 标注），并跑**真正放开** `{0.5,1.0}` 的
  key 饱和确认（现是 `NODE_CAP=100M` 下界）——可加"连续 N 万节点无新 key 即判饱和"早停，或临时抬高
  `NODE_CAP`。然后才谈把 B3 接进 `pack_info_set` / dense indexer 的实现（§B3 改动面）+ HU 上 exploitability/LBR
  对 `{1.0}` perfect-recall baseline 验质量（imperfect recall 收敛无保证，须实测裁定）。

**【2026-05-31 调研新增——Phase 0 便宜 probe，应先于任何买机器/选 B3 的决定】**

- (e) **【新】WIDTH_CAP probe（§A4）**：`nlhe_betting_tree_sizing` 枚举 `{0.5,1}` 但剪掉 preflop 后 >N 人
  （N=2/3）在场的节点，量 node/infoset/reached。验"6 人各自再加注"的 width 是不是病根、~20× 能否复现。
- (f) **【新】raise-index/街 菜单 + reached-set probe**：`legal_bet_sizes` 做成 `(street, raise_index)` 纯函数
  （0.5 仅每街首次加注 / 仅 turn-river），enumerate **并**量 C5 后端的 **reached set**（不只 enumerated dense
  字节）。测"小注不可行"是不是平菜单 artifact + reached 是否把内存拉进 64–96 GiB（Pluribus reach 62%）。
- (g) **【强化 (d)】B3 `{0.5,1}` 真 distinct-key**：cap sweep 已证 `NODE_CAP=100M` **未饱和**（每翻倍 ×~1.9），
  现 ~4.9 GiB 不可用于定机器。用更大 cap / key-only walk 重测真值（high-10⁵~10⁶?），再定 dense GiB。
- (h) **【新】Phase 2 正交收益**：① `map_off_tree`（`action.rs:386` nearest-ratio stub）升 **pseudo-harmonic**
  （`f(x)=(B-x)(1+A)/((B-A)(1+x))`，正确 off-tree handler，但不压 key、不替代 key 决策）；② **VR-MCCFR
  control-variate baselines** 治 A3 真瓶颈（训练 wall 7.5×，文档原称无解）；③ **A-loss recall 断言 +
  range-skew(KL/EMD)** 把 B3"糊掉多少 range"量成训练前数字（用现成 `action_probs` 日志）。

## 参考

**完整带注释 + 核到原始来源（含 URL、关键数字）的文献表见
`betting_history_abstraction_research_2026_05_31.md` §6**：含 Modicum（depth-limited）、Lisy-Bowling
（translation 可剥削 ~4020 mbb/g）、FPIRA/CFR+IRA（自动 bounded-loss IR）、A-loss recall、Fu 2025/KrwEmd
（higher-resolution IR）、RL-CFR、VR-MCCFR、GTO Wizard（width cap / size EV / QRE）等本次新增引用。下面保留原表。

- Waugh et al. 2009 *A Practical Use of Imperfect Recall*（CFR + imperfect-recall 抽象的奠基工作）。
- **Kroer & Sandholm, EC'16 *Imperfect-Recall Abstractions with Bounds in Games***（CRSWF 游戏类 + 合并
  infoset 的解质量上界 = reward error + chance error；B3 拿 SPR 桶当 key 的理论依据）。推广自 Lanctot et al.
  2012 skew well-formed games。
- **Johanson et al. AAMAS'13 *Evaluating State-Space Abstractions in Extensive-Form Games***（imperfect recall
  同内存下 exploitability + 单挑都胜过 perfect recall——但其 imperfect recall 摘的是**牌**不是下注历史）。
- **Brown & Sandholm 2019 *Superhuman AI for multiplayer poker*（Pluribus）supp**：对下注历史用**完美回忆**
  （bucket iff 同 action-abstraction sequence + 同牌桶），1–14 档每街递减、lazy 分配、6.65 亿动作序列；
  **不**摘下注成 SPR 桶——B3 的"摘下注"是更激进、业界少见的路子。
  **【验证纠正】**：blueprint 是 **full-depth root-to-river MCCFR**（"per-round 叶子在下街 chance"是
  **real-time search 专属**，不是 offline 分解，调研多次说反）；664,845,654 序列实 **reach 413,507,309（62%）**；
  机器 **<0.5TB / 64-core / 8 天 / $144**（= A3 的 512GB 级，非 64 GiB）；rounds 3-4 首次加注 `{0.5p,1p,allin}`、
  后续 `{1p,allin}`（= raise-index 菜单，§A1/§A3）；round 1 最细（因不 search）。
- DeepStack（Moravčík et al. 2017）/ 商用 solver（GTO Wizard / PioSOLVER）：续局价值的充分统计量取
  `(range, 底池/SPR, 公共牌)`，preflop 序列压成起手底池——B3 "SPR 桶 = key" 的业界先例（限于跨街折叠）。
- 本项目：`docs/six_max_nlhe_target.md` §S2（树规模实测）；`src/training/nlhe_betting_tree.rs`（node_id 完美回忆树）；
  `src/training/nlhe.rs:pack_info_set_v2`（v2 packer，弃用 position/stack/betting_state）；`src/training/nlhe_dense.rs`
  （按 node_id 的 dense indexer）；`src/abstraction/info.rs`（`BettingState` 5 状态 + InfoSetId layout）；
  `src/abstraction/map/mod.rs`（位编码 + D-252 禁浮点）。

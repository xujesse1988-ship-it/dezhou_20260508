# 下注历史抽象表示方案（6-max，betting tree 之外的选项）

工作笔记。背景：S2 实测发现显式 betting tree 在 6-max 多 bet size 下爆炸
（`{0.5,1}` > 1 亿节点 / ≥20B infoset / ≥645 GiB，见 `docs/six_max_nlhe_target.md` §S2）。
本文列出"除 betting tree 外，下注历史还能怎么抽象表示"的方案谱系，供定 6-max 抽象时取舍。

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

### A2. Public-state 规范化（transposition 折叠）
把"到达相同 `(各家已投筹码, 该谁动, 本街已加注数)`"的不同动作序列并成同一节点。完美回忆下它们
本是不同节点，合并后是 transposition 折叠。无损或近无损，但要算"状态等价"而非"路径"，实现复杂度高于 A1。

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

**设计决定（本轮敲定）**：

- `live_players` 用 **bitmask 不用计数**——6-max 里 button 在 vs UTG 在剩余 range/位置完全不同；硬约束
  `actor_position ⊂ live_players` 砍掉大量非法组合。不另花 bit 区分 Active/AllIn（深度后果已被 `spr_bucket` 吃掉）。
- `last_aggressor` 从"本街 1 个"扩成 **2 槽（preflop + postflop 线），相对 button**——过去街 aggressor 可能已弃牌，
  相对 actor 的"前/后"会坏，相对 button 是固定坐标系才记得住。**删掉"我是否 aggressor"**（= `last_aggressor==我` 可
  导出）和单独 initiative bit（已被 `preflop_aggressor` 槽吸收）；只记 2 槽（PFR + postflop 线）而非满 4 街，避免滑向完美回忆。
- **两个作用**：① `raises_this_street` + `facing_size_bucket`（含"无活注"）**必须**完整决定合法动作集，否则重蹈
  F17（同 key 但合法动作不同 → regret 矩阵错位）；② `spr_bucket`（物理量）+ `live_players` + `last_aggressor`
  （range/initiative）逼近续局价值。
- **跨街信息只靠 `spr_bucket` / `live_players` / `last_aggressor` 三个累积字段扛**；其余只看当前街。**仍丢**：精确路径、
  桶外精确尺寸、街内逐手次序、fold 先后——这是 imperfect recall 的有损部分。

**位预算**：下注摘要 = 3+6+2+3+4+6 = **24 bit**，装进 v2 弃用的 `position(4)+stack(4)+betting_state(3)`+reserved 26
= 37 bit 可用；全 key（含 `bucket_id` 24 + `street_tag` 3）≤ 51 bit < 64，富余。

**key 空间 / infoset 估算（量级，待 sizing 工具实测坐实；定稿字段比初版更富，规模上移）**

- 定稿字段（bitmask `live_players` + 2 槽相对-button `last_aggressor`）的**原始**笛卡尔积比初版大不少（单街粗估
  `6 × ~30(actor×在场集) × 4 × 5 × 12 × ~20(2 槽 aggressor) ≈ 10⁵–10⁶/街`）；但硬约束狠砍可达集：`actor⊂live`、
  aggressor 必属当街在场、无活注 ⇒ `raises=0`、底池小 ⇒ SPR 不可能极高。
- **HU（首个验证目标）极小**：`live_players` 恒满、aggressor 槽各 1 bit、actor 0..1 → 下注 key 仅几百~几千/街，
  dense 两表必然 ≪ 1 GiB，先在 HU 上把不变量 + 收敛质量验明白。
- **6-max 是真问题**：bitmask + 2 槽是"保真度换大小"的两个旋钮，把 key 空间推向大端，**6-max 绝对规模必须实测**
  才能断言能否进 64 GiB 小机——这正是当初警告的"滑向完美回忆"的代价，不再给初版那个乐观的 ~1600 万 infoset 单点估计。
- 不变的定性结论：key 空间**有界且与 bet-size 菜单几乎无关**（`{0.5,1.0}` 只把 SPR/尺寸桶填密，不像树那样组合
  爆炸），故 B3 下 `{0.5,1.0}` **不再像 node_id 那样 ≥645 GiB**；"具体几 GiB / 进不进小机"是 6-max 实测题
  （待办 c）。这是 B3 相对 A3（首选但要 ~512 GB 大机）的根本方向——**有望小机保住小注尺寸，量级待坐实**。

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
- **A 类（raise cap / 状态规范化 / first-bet-small）风险最低、无损、能与 B/C 叠加**。对小注：单机 64 GiB
  预算下 raise cap 救不了（§A1，仍 199 GiB）；first-bet-small 无损保住小注尺寸但要 **~512 GB 大机**（§A3，
  224 GiB 两表）——即"堆内存换无损"，可行但贵。

## 对我们最对症的组合

爆炸 = **re-raise 深链（A1 可砍）+ 小注底池几何导致的多路深筹码续局（结构性削减砍不动深度，见 §A1/§A3 实测）**。
想要 postflop 小注尺寸，三条路按"机器 vs 有损"取舍：

- **(i) ~512 GB 大机 + first-bet-small（§A3）**：无损保留完整路径，纯堆内存，224 GiB 两表落进 512 GB 富余。
  代价 = 大机 + 训练 wall（infoset 7.02B ≈ `{1.0}` 的 7.5×）。**当前倾向先走这条摸质量上限**（无损、不改语义）。
- **(ii) 小机 + B3 摘要**（详细设计见 §B3 草案）：`(actor_position, live_players, raises_this_street,
  facing_size_bucket, spr_bucket, last_aggressor)` 替掉 `node_id` 完整路径 → key 空间有界（估 ~3–4 万 key /
  ~1600 万 infoset / dense 两表 <1 GiB，待实测）、爆炸消失且与档数无关。本质 = 对下注历史做 imperfect-recall。
  代价 = **有损 + 弱收敛**（bot 不再区分"底池怎么变大的"，CFR 完美回忆保证失效，须实测裁定）+ 改 InfoSetId
  语义。是把 (i) 省成小机的路子；理论有界（Kroer-Sandholm CRSWF）但摘下注历史业界少见，比"摘牌"激进。
- **(iii) 直接弃小注**：少量大档 + A1（`{1.0,2.0}` cap=1 已进 64 GiB），最省事，放弃 postflop 小注尺寸。

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
- (c) **实测 B3 key 空间**（把 §B3 估算坐实）：在 `tools/nlhe_betting_tree_sizing` 复用现成 `walk`，每访问一个
  tree 节点算 `betting_summary_key`，用 HashSet 数 distinct `(summary_key, street)` 与 infoset 数；对
  `{1.0}` / `{1.0,2.0}` / `{0.5,1.0}` 各跑一遍。同时断言"同 key 节点 `legal_actions` 一致"（验不变量 3）。
  这是把"设计"变"数字"的最小实验，不动训练路径。

## 参考

- Waugh et al. 2009 *A Practical Use of Imperfect Recall*（CFR + imperfect-recall 抽象的奠基工作）。
- **Kroer & Sandholm, EC'16 *Imperfect-Recall Abstractions with Bounds in Games***（CRSWF 游戏类 + 合并
  infoset 的解质量上界 = reward error + chance error；B3 拿 SPR 桶当 key 的理论依据）。推广自 Lanctot et al.
  2012 skew well-formed games。
- **Johanson et al. AAMAS'13 *Evaluating State-Space Abstractions in Extensive-Form Games***（imperfect recall
  同内存下 exploitability + 单挑都胜过 perfect recall——但其 imperfect recall 摘的是**牌**不是下注历史）。
- **Brown & Sandholm 2019 *Superhuman AI for multiplayer poker*（Pluribus）supp**：对下注历史用**完美回忆**
  （bucket iff 同 action-abstraction sequence + 同牌桶），1–14 档每街递减、lazy 分配、6.65 亿动作序列；
  **不**摘下注成 SPR 桶——B3 的"摘下注"是更激进、业界少见的路子。
- DeepStack（Moravčík et al. 2017）/ 商用 solver（GTO Wizard / PioSOLVER）：续局价值的充分统计量取
  `(range, 底池/SPR, 公共牌)`，preflop 序列压成起手底池——B3 "SPR 桶 = key" 的业界先例（限于跨街折叠）。
- 本项目：`docs/six_max_nlhe_target.md` §S2（树规模实测）；`src/training/nlhe_betting_tree.rs`（node_id 完美回忆树）；
  `src/training/nlhe.rs:pack_info_set_v2`（v2 packer，弃用 position/stack/betting_state）；`src/training/nlhe_dense.rs`
  （按 node_id 的 dense indexer）；`src/abstraction/info.rs`（`BettingState` 5 状态 + InfoSetId layout）；
  `src/abstraction/map/mod.rs`（位编码 + D-252 禁浮点）。

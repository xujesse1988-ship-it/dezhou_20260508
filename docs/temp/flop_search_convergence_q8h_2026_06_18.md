# 实时搜索翻牌子博弈（4-way/深码）不收敛：Q8h「121BB 梭哈 gutshot」证伪（2026-06-18）

> 中性记录。是 `river_search_convergence_2026_06_17.md`（河牌）、`turn_search_convergence_ac4c_2026_06_18.md`（转牌）的续作，把同一套诊断推进到**翻牌**街。
> 一句话定论：**live 在 4-way 翻牌把 121BB（13.5× 池）梭哈给一个 gutshot（allin 0.695）—— 是欠收敛假象，确认。** 三件独立证据（跨 seed @100M / 跨预算轨迹 / 单 run 内轨迹）全部指向：这个 4-way 翻牌子博弈在可行预算内**根本不收敛、不可识别**，唯一稳健的结论是 `allin≈0`。根因结构性：翻牌子博弈（后压两条街、几万 infoset、最多 40M 节点）太大，12s/4线程 live 预算解不开。**可剥削性在此也算不出可信值**（见 §4）。

---

## 0. 起因

用户在 openpoker live 抽到一手，直觉「拿这种牌不该这么打」：

- 6-max 10/20，btn=seat2，**hero=seat3（SB）持 `Qh 8h`**。
- preflop UTG(seat5) open45 → seat0/1 fold → btn(seat2) call → **hero(SB) call** → BB(seat4) call。**4-way 见 flop**（UTG / btn / SB-hero / BB）。
  - ⚠ **hero 是用 blueprint 弃 98.58% 的牌进的池**（resp probs `fold 0.9858 / call 0.0142`，live 抽到 1.42% 的 call）→ 翻牌脱锚 range 重建里 Q8s 权重极低，本身带畸变。
- **flop `Tc Ad 9h`**：hero = J 的 gutshot（8-9-T-Q 缺 J，~4 张）+ Q 高 + 后门花（Qh8h+9h）。弱听牌、OOP、4-way。
- **live 决策**：`source=search:unanchored`，输出 **allin 2431 内部≈121BB**，进 ~180 池（9BB）= **13.5× 池超池梭哈**。probs `check 0.057 / bet0.5 0.142 / bet1 0.107 / allin 0.695`，`solve_updates 1.27M`。

## 1. 配置与工具

分支 `tmp-subgame-exploit`，本次新增两个 commit 把诊断仪表接进**脱锚（unanchored）路径**（之前只在 anchored 路径生效）：
- **`74e5686`**：`SIX_MAX_EXPLOIT_KDEALS` 抽成 `report_subgame_exploit` helper，unanchored 也调用（`mc_exploitability` 本即 N-player BR，4-way 直接可算）。
- **`d5b942d`**：`SIX_MAX_TURN_TRACE_EVERY` 中途快照接进 unanchored（脱锚 round-start 子树根 == hero 决策节点，末次快照 == 末尾 probs，自带校验）。

参数（**对齐 live config**，与转牌研究不同处见下）：
```
--checkpoint .../nlhe_es_mccfr_final_010000000000.ckpt   # 10B preopen
--bucket-table bucket_table_200_200_200_...               # blueprint 桶
--reshape preopen
--search --search-trigger all-postflop --search-max-nodes 40000000 --search-deep-menu
--search-bucket-table bucket_table_default_1000_1000_1000_...   # 搜索桶 1000（新默认）
--search-lcfr --search-solve-threads 4                    # LCFR 臂、4 线程
--seed {1,2,3} --search-iterations {5M..100M}             # 固定迭代
```
- **与转牌/河牌研究的差异**：① 4 线程（非单线程）——翻牌树太大、单线程 100M ~80min/run，4 线程对齐 live 且可行（代价 = stale-σ 混淆，但本结论是「不收敛」，stale-σ 只会让收敛看上去更差，不影响判决方向）；② `max-nodes 40M`（对齐 live，非 4M）；③ 只跑 LCFR 臂（对齐 live `--search-lcfr`，未做 uniform 臂——跨 seed 不一致已足够定性）。
- **单位**：子树内部 chip = OpenPoker 5×（hero jam OpenPoker 2431 → 内部 12155）。1 BB = 20 OpenPoker chip = 100 内部 chip。
- **树规模/慢**：引擎自标 `SLOW`，~28–47µs/迭代（转牌 ~11–33µs），100M ≈ 47min/run（4 线程），RSS ~7.7GB。

## 2. 结果：三件独立证据，全部指向「不收敛」

### 2.1 跨 seed @100M —— 完全不可识别

| seed | check | bet0.5 | bet1pot | allin |
|--:|--:|--:|--:|--:|
| 1 | 0.060 | 0.191 | **0.750** | 0.0 |
| 2 | 0.474 | 0.039 | 0.487 | 0.0001 |
| 3 | **0.587** | 0.198 | 0.215 | 0.0 |

三 seed 横跨 **check 0.06 → 0.59、bet1pot 0.22 → 0.75**：seed1 几乎纯打池、seed3 几乎纯过牌、seed2 居中。**到 100M 跨 seed 还各打各的。** 对比转牌 Q8s… 不，对比上一手转牌（100M 三 seed 全 check≈1.0）—— 那是收敛，这是不收敛。

### 2.2 跨预算轨迹（seed1，各为独立 run）

| 预算 | check | bet0.5 | bet1pot | allin |
|--|--:|--:|--:|--:|
| **live 1.27M** | 0.057 | 0.142 | 0.107 | **0.695** |
| 2M | 0.126 | 0.198 | 0.394 | 0.282 |
| 5M | 0.959 | 0.021 | 0.020 | 0.0004 |
| 20M | 0.306 | 0.316 | 0.378 | 0.0002 |
| 100M | 0.060 | 0.191 | 0.750 | **0.0** |

check 0.96→0.31→0.06，bet1pot 0.02→0.38→0.75，到 100M **还在单调漂**。`allin` 从 live 的 0.695 直接掉到 ≈0 并钉死。

### 2.3 单 run 内轨迹（seed2，traced，每 1M）

| updates | check | bet0.5 | bet1pot | allin |
|--:|--:|--:|--:|--:|
| 1M | 0.915 | 0.004 | 0.016 | 0.065 |
| 2M | 0.358 | 0.606 | 0.021 | 0.015 |
| 3M | 0.171 | 0.728 | 0.094 | 0.007 |
| 4M | 0.152 | 0.639 | 0.205 | 0.004 |
| 5M | 0.097 | 0.670 | 0.230 | 0.003 |

**一个 run 内**就剧烈摆动（check 0.92→0.10、bet0.5 0.004→0.67、allin 0.065→0.003）。5M 末次快照 == 最终 probs（`check 0.097/bet0.5 0.670/bet1 0.230/allin 0.003`），自带校验通过 → 工具正确。

## 3. 判决

**live 的 `allin 0.695`（121BB 梭哈给 gutshot）= 欠收敛假象，确认。**
- `allin≈0` 在**所有**干净预算 / 所有 seed 都成立（唯一稳健结论）；那个 13.5× 超池梭哈在任何诚实解里都不存在。
- live 0.695 = **1.27M 更新（12s 预算）+ 4 线程 stale-σ + prewarm** 在一棵远未解开的树上凑出的噪声。
- 连「该过牌还是该下注、下多大」都没识别（三 seed 从纯过牌到纯打池）—— 这个节点在可行预算内**没有可报告的均衡策略**。

**根因（结构性）**：4-way 翻牌子博弈后面压着转牌+河牌两条街、几万 infoset、最多 40M 节点、~28–47µs/迭代。12s/4线程 live 预算连边都摸不到。对比：HU 河牌子博弈 ≈0 可剥削、HU 转牌子博弈 100M 收敛、这手 4-way 翻牌 100M 仍发散。叠加 hero 用 blueprint 弃 98.6% 的牌进池 → 脱锚 range 畸变。

## 4. 为什么可剥削性在此算不出可信值

> **口径更正（2026-06-18，见 §7）**：本节标题失之绝对。准确说是「**deal-MC 全树 BR 这个估计量、在 4-way、单线程可行 k 下**算不出」——可剥削性本身**可计算**（该估计量相合、k→∞ 收敛真值；负值=BR 欠训练；k-sweep 取平台即真值）。本节记录的是 k=200 单线程实测，结论范围按 §7 收紧。

接线已补（§1 `74e5686`），但对这个 4-way 翻牌节点跑出的数**不可用**，双重原因（非缺 hook）：
1. **可行 k 下结果是数学上不可能的负值。** k=200 实测 `expl_sum = −16.1 BB`（内部 −1614.9）。可剥削性按定义 `Σ_i[BR_i − u_i] ≥ 0`；负值 = BR 在这棵巨大的 4-way 三街树（几万 infoset）上**严重欠训练**（200 个 deal 访问不到几个 infoset，BR≈噪声、在独立 eval deal 上打得比 σ̄ 还低）。`detail` 四个座位 BR_i < u_i 全负坐实。
2. **可信 k 单线程算不动。** 转牌（2-way、~2900 infoset）要 k=8000；此翻牌树大得多。而 BR 单线程，k=200 一趟就 ~55min（整进程 64min）；拉到可信 k 要数小时/趟，不现实。

→ **多人/翻牌大子博弈的可剥削性，现有工具（单线程 deal-MC、可行 k）实质算不出**（turn/river 那种 2-way 节点能算，k=8000 没问题）。本判决因此不依赖可剥削性，靠 §2 三件收敛证据（足够）。**但「算不出」≠「不可计算」——取真值的方法见 §7。**

## 5. 实战含义

**实时搜索在「多人 + 翻牌 + 深码」这类大子博弈上不可信** —— 子博弈太大、预算内解不开，输出偏激进（这次直接梭哈坚果级下注量给一个 gutshot）。这是要正式记下的**能力边界**，不是单点 bug。可能的缓解方向（未做）：翻牌街限深/限宽、菜单收窄（多人禁超池/禁梭）、preflop 不该进的池（hero 弃 98.6%）从源头掐掉。

## 6. 复现

```bash
# req: q8h_flop_req.json（flop 决策，hero=SB 首行动，4-way）
SIX_MAX_TURN_TRACE_EVERY=10000000 [SIX_MAX_EXPLOIT_KDEALS=200] \
./target/release/openpoker_advisor \
  --checkpoint artifacts/run_6max_s4_preopen_n3_10b/nlhe_es_mccfr_final_010000000000.ckpt \
  --bucket-table artifacts/bucket_table_200_200_200_seed_cafebabe_schemav4.bin --reshape preopen \
  --search --search-trigger all-postflop --search-max-nodes 40000000 --search-deep-menu \
  --search-solve-threads 4 --search-bucket-table artifacts/bucket_table_default_1000_1000_1000_seed_cafebabe_schemav4.bin \
  --search-lcfr --seed S --search-iterations N < q8h_flop_req.json 2>err.txt
# TRACE_TURN 快照在 err；最终 probs 在 stdout 末行；EXPLOIT_* 在 err（多人翻牌不可信，§4）
```
vultr `~/dezhou_20260508/` 产物：`q8h_flop_req.json`、`run_flop_study.sh`、`flop_conv/s{1,2,3}_100M.{out,err}`、`flop_conv/s1_{05M,20M}.*`。工具分支 `tmp-subgame-exploit`（`d5b942d`：exploit + trace 接进 unanchored）。

---

## 7. 可剥削性怎么算——方法学（2026-06-18 续；修正 §4 的「算不出」）

> 把「翻牌/多人子博弈的可剥削性到底怎么算、§4 的负值是什么、取真值有哪几条路」一次讲清。**结论先行**：可剥削性**可计算**；§4 的「算不出」要限定到「**单线程 deal-MC BR、可行 k**」。

### 7.1 定义、两半、常和

$$\mathrm{expl}(\bar\sigma)=\sum_i\big[BR_i(\bar\sigma_{-i})-u_i(\bar\sigma)\big]\ \ge 0,\quad =0\ \text{在 NE}$$

- 子博弈是**常和**（弃牌座死钱 = 常数），非零和 → 必须用 `Σ_i(BR_i−u_i)`，不能套 HU 的 `(BR_0+BR_1)/2`（工具注释明说；与既有 `exploitability::<G,BR>` 的差只来自零和下 `u_0+u_1=0`，故 `mc=2×`）。
- **`u_i` 半（易、已解决）**：全员打 σ̄、平均整手净筹码（`mc_profile_value`）。**每副 deal 都贡献 → 全覆盖、任意 k 都准**。本研究 `ROOT_U` 实测（s1@5M，k=5万）：`u=[+0.52, −0.28, −1.25, +1.00] BB`，SE≈0.08。`Σ_i u_i` = 死钱。
- **`BR_i` 半（难、问题全在这）**：固定其余座为 σ̄，求座 i 的最优反应**值**。

### 7.2 结构约束（决定能用什么方法）

- 子博弈**无树内 chance 节点**：底牌 + 整条 board（turn+river）在 `root()` 一次性发，betting tree 是纯决策/终局（`nlhe.rs` `current` 只返回 Player/Terminal，`chance_distribution` 直接 panic）。**实测树仅 45,431 节点**（`--search-max-nodes 40M` 是上限、非实数）。
- infoset = `(公开历史, 桶)`；桶由 `(hole, board)` 定，1000 桶/街。
- 两个推论：
  1. 既有**精确** `exploitability::<G,BR>`（枚举 in-tree chance、Kuhn/Leduc 精确）**用不了**——这里 chance 全在 root，对单副**已知牌**做一次 walk = **开天眼（clairvoyant）best response**，跨 deal 平均得 `E[max]`（Jensen 上界，不是可剥削性）。
  2. 只能**deal-积分**：采 deal、把 per-`(infoset,action)` cfv 累进同一张桶-key 表、提交**单一动作**（非 per-runout）→ 去开天眼。

### 7.3 `mc_exploitability`（deal-MC BR）：相合 + k=200 为何出负

- **算法**：`k_train` 副 deal，每副一次**全树 `walk_cfv`** 累积 `cfv[I][a]`，policy-iteration argmax → BR 单动作策略；在**独立** `k_eval` 副评估其值（去 in-sample 过拟合）。
- **相合（consistent）**：`k→∞` 时 `cfv[I]` → 真反事实值 → argmax → 真 BR。**硬证** = 对 in-tree-chance 的 Kuhn 与精确 `exploitability::<KuhnGame>` 逐数吻合 `<1e-9`（单测 `mc_exploitability_matches_exact_kuhn`）。**方法正确，错的只是有限 k。**
- **覆盖机制（4-way 为何难、k=200 为何负）**：infoset `(历史, 桶 b)` 只能从「actor 手在该历史落进桶 b」的 deal 拿训练信号。1000 桶/街 × 几千公开节点 → infoset 量级几万；k=200 副 deal 下绝大多数 0–1 命中 → BR 在那里退 **uniform 默认** → 这种「BR」在独立 eval deal 上**打得比 σ̄ 还差**（uniform 劣于受训 σ̄）→ `BR_i−u_i<0`、四座全负、`Σ=−16.1 BB`。**负值 = BR 严重欠训练，不是不可计算。**
- **下界 + 趋势单调**：disjoint-eval 使 `expl_est(k)` 是真可剥削性的**下界**（受训 BR 欠优 → 其无偏 eval 值 `≤` 真 BR 值），且随 `k_train` **趋势单调上升**（覆盖↑ → BR less 欠训练 → 值爬向真 BR）。**轨迹 = 负 → 0 → 平台；平台值 = 真可剥削性。**
- **2-way 是同一台仪器的成功案例**：turn/river k=8000 已落在平台（expl 0–2 BB），因 reachable infoset 少（~160–19000）、deal 自然集中在高 reach 处。**4-way 翻牌只是 infoset 更多、reach 更稀 → 平台需要更大 k**——这就是 §4 在 k=200 看到负值的全部原因，不是结构性不可算。

### 7.4 瓶颈（= §4.2 的实质）

BR **单线程** + 每副 deal 是一次 **O(树=45k 节点)的全树 walk**（含 `state.clone`）。大 k 单线程慢。**不是内存 / 迭代吞吐瓶颈，是覆盖 × 单线程**：到平台需 k 大到「每个有 reach 的 infoset 被够多 deal 命中」。

### 7.5 取真值的三条路（数学 / 成本 / 何时用）

**A. 并行 deal-MC + k-sweep（首选）**
- deal 间彼此独立 → rayon 并行 `walk_cfv`、合并 per-infoset cfv（求和可交换）；eval 同样尴尬并行。
- k-sweep **既产数值又自证收敛**（看 `expl_est(k)` 是否走平）。
- 成本 ≈ 线性于 `k/核数`；32 vCPU 训练机上大 k（几万–几十万）可行。
- 风险：4-way 平台 k 可能很大；但并行后只是 wall-clock，不是不可算。
- **复用 Kuhn 已验证的工具、零新算法 → 默认首选。**

**B. range-based 精确 BR（最干净、最贵）**
- 一趟自底向上：per-`(座,桶)` reach 向量沿公开树传播，叶子按 **N-way 去牌**（inclusion-exclusion over 3 对手）折反事实值，target 节点取 max → **精确 BR、构造即非负、无 k 无采样、确定性**。
- 难点 = N-way 去牌（HU 是标准 1326-vector，4-way 要扩到 3 对手的容斥）；且当前「chance 全在 root」的设计与它对着干（board 要么改回树内 chance、要么按 runout 枚举折桶）。
- **何时用**：要 ground-truth、或 A 在可行 k/核数下到不了平台、或要可重复的精确数。多天工程。

**C. LBR 下界（最便宜、已存在）**
- `estimate_lbr`（`lbr.rs`）**已 Game-generic、现在就能跑 `SubgameNlheGame`**：probe 决策点 → 枚举动作 → 各动作后 σ̄ rollout 取 max。
- **非负下界**（期望意义），不犯全树覆盖（只在一点偏离）。
- 弱点：probe 后回 σ̄ → 抓不到**多街**剥削 → **LBR≫0 能坐实可剥削，LBR≈0 不能反证不可剥削**；`target=probe_idx%2` 写死 HU，4-way 要推广 N 座。
- **何时用**：要快速「至少这么可剥削」的非负证据 / 给 A、B 做交叉校验。

### 7.6 判读规则（方法学要点，来自姊妹文档）

- **可剥削性是判据，频率不是**：频率跨 seed 分裂 + 可剥削性低 = 非唯一均衡（健康，KdAc）；+ 可剥削性高 = 欠训练（4hJh 500 桶）。
- **值收敛（`u_i` 跨 seed）是配套判据**，cheap、全覆盖（`ROOT_U` 已能给）；值唯一 + 频率不唯一 = 2p0s 可互换流形。
- **单位**：1 BB = 100 内部 chip（4hJh 旧记系 5× 单位错）。
- **桶敏感**：500 vs 1000 桶能造「假地板」（4hJh §12）；诊断默认 1000 桶。

### 7.7 结论

可剥削性**可算**；§4 的「算不出」限定到「单线程 deal-MC、可行 k」。`u` 半已解（`mc_profile_value`/`ROOT_U`，全覆盖）；`BR` 半取真值 = **A 并行 k-sweep（首选，复用验证过的工具、自证收敛）** / **B 精确 range-BR（最干净最贵）** / **C LBR（最便宜的非负下界）**。

**仪表现状**（分支 `tmp-flop-root-values`，origin 已推，从 `tmp-subgame-exploit` d5b942d 起）：已加 `SIX_MAX_ROOT_VALUES`（root per-action cfv + 各座范围值，σ̄ rollout、O(depth)、全 k 覆盖；hero 真实手 pinning 解决 §0 桶权重≈0 的 cnt=0）。**A 的 BR 并行 + k-sweep、B、C 均未做**（本节是方法学，未跑取真值）。

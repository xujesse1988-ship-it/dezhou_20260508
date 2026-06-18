# 实时搜索河牌子博弈收敛性 + 可剥削性诊断：QsJc（近纯）+ KdAc（混合）（2026-06-17）

> 中性记录。是 §10/§11（4hJh **转牌**子博弈，`turn_search_convergence_4hJh_2026_06_16.md`）的续作，把同一套工具搬到**河牌**节点上验。
> 一句话定论：**河牌子博弈在生产级预算下能干净收敛到 ≈0 可剥削（与 4hJh 转牌 500 桶下卡 ~2 BB 相反；注：4hJh 旧记 ~10 BB 系 5× 单位错，且 1000 桶证为抽象受限非硬地板，见其文档 §12）；混合河牌点（KdAc）的策略跨 seed 完全不收敛（梭哈 4%–87%），但博弈值（−6.4 BB）和可剥削性（<0.7 BB）都收敛——这是真·非唯一均衡，不是 4hJh 那种欠收敛。**

---

## 0. 起因

用户在另一台机（非 vultr，12 线程墙钟 8s）跑 openpoker live。先送来一手 **QsJc** 的河牌决策（live 抽到 allin），问「在河牌重建子树、3 seed × 100M、每 10M 快照、看是否收敛，并用 deal-积分 MC exploitability 算可剥削性」。

QsJc 收敛后发现是**近纯策略**（下注 ~98%），用户指出「近纯点收敛是 trivial 的」，要求再找一个**河牌阶段的混合点**复测——得到 **KdAc**（A 高在 `TT` 配对面的 give-up/诈唬 4 路混合）。本文记两点位的全部过程与结论。

与 4hJh 的关系：4hJh 是**转牌**根（下面还压着河牌发牌 + 河牌下注两层），可剥削性卡 ~200 内部 chip（= **~2 BB**；1 BB=100 内部，旧记 ~10 BB 系 5× 单位错；且 2026-06-18 1000 桶实验证明可降、是 500 桶抽象受限非硬地板，见 4hJh 文档 §12）；本文测的是**河牌**根（牌面已 5 张全发、下面只剩一轮下注、无发牌节点）。

## 1. 配置与工具

分支 `tmp-subgame-exploit`（commit `9ce664f`，origin 已推）= 4hJh §10/§11 用的**同一套**诊断工具，未并入 6max：
- **`SIX_MAX_TURN_TRACE_EVERY=<updates>`**：solve 过程中每 N 个 update 把决策点平均策略打到 stderr（`TRACE_TURN` 行）。门控 = `within_round_tags 为空（轮起点）+ deep_menu`，**与街无关**（名字带 TURN 是历史遗留）；河牌轮起点同样触发。
- **`SIX_MAX_EXPLOIT_KDEALS=<K>`**：solve 完对 σ̄ 算 **deal-积分 MC exploitability**（`mc_exploitability`，Kuhn 精确单测吻合 <1e-9）。`k_train` 个 deal 求 BR、独立 `k_eval` 个 deal 评估（去过拟合 → 报值是下界）。同时打 `EXPLOIT_CUR`（current 策略可剥削性）、`EXPLOIT_STREET`（按街拆）、`EXPLOIT_STREETSTATS`。

公共参数（与 live 对齐，差异见下）：
```
--checkpoint artifacts/run_6max_s4_preopen_n3_10b/nlhe_es_mccfr_final_010000000000.ckpt   # 10B preopen blueprint
--bucket-table artifacts/bucket_table_200_200_200_seed_cafebabe_schemav4.bin
--reshape preopen
--search --search-trigger all-postflop --search-max-nodes 4000000 --search-deep-menu
--search-bucket-table artifacts/bucket_table_default_500_500_500_seed_cafebabe_schemav4.bin   # 搜索桶 500
--search-solve-threads 1            # 单线程：对齐 §10.3 可剥削性方法 + 去并行 stale-σ 混淆
[--search-lcfr]                     # uniform 臂关、LCFR 臂开
--seed S --search-iterations 100000000   # 固定 100M（period=2M、50 次 rescale）
SIX_MAX_TURN_TRACE_EVERY=10000000 SIX_MAX_EXPLOIT_KDEALS=8000
```
- **设计**：每点位 **3 seed（1/2/3）× 100M × {uniform, LCFR} = 6 run**，每 10M 快照 + 8000/8000 deal 可剥削性。**串行**（每进程加载 5.9G blueprint → ~7G RSS，vultr 11GiB 一次只放一个）。每 run ~10–12 min。
- **单位**：子树内部 chip = OpenPoker 的 **5×**（KdAc 全下内部 9705 = 真栈 1941×5；QsJc 6965 = 1393×5）→ **1 BB = 100 内部 chip**。下文 BB 均按此换算。
- **与 live 的差异（= 不是 live 那一次的精确重放）**：固定迭代 vs 墙钟 8s；单线程 vs 12 线程；自选 seed vs 未知 base_seed；无 prewarm。研究的是**节点的收敛性质**，非复现某次决策。

## 2. QsJc 河牌（近纯策略点）

**局面**：6-max 10/20，btn=seat5，hero=seat0（SB）。preflop BTN(seat5) raise44 → hero(SB) call → BB fold；flop `Kd5cAs` hero bet108 → BTN call；turn `Kc` hero check → BTN bet233 → hero call；river `Ad`（牌面 `Kd 5c As Kc Ad` = A-A-K-K-5）hero 先行动（轮起点）。`info_set 2965760817234059`，菜单 `[check,0.5pot,1pot,allin]`，有效栈 ~89BB（hero 1393 剩余）。
- **hero 牌力 = 坚果两对**：QsJc 用牌面 AAKK + Q 当踢脚 = AAKKQ，是**非葫芦里最强的牌**（Q 是最佳踢脚，要打过 hero 须有 A/K 或口袋 55）。
- **live 决策**：`source=search`，抽到 **allin**（`bet1pot 0.6881 / allin 0.3117`，solve_updates 6,481,152）。

**100M 收敛结果**（`check / 0.5pot / 1pot / allin`，hero v 来自 u_hero）：

| 臂 | seed | check | 0.5pot | 1pot | allin | expl σ̄ (BB) |
|--|--:|--:|--:|--:|--:|--:|
| uniform | 1 | 0.000 | 0.000 | 0.956 | 0.044 | −0.14 |
| uniform | 2 | 0.000 | 0.001 | 0.987 | 0.012 | +0.32 |
| uniform | 3 | 0.000 | 0.000 | 0.985 | 0.014 | +0.20 |
| LCFR | 1 | 0.000 | 0.000 | 0.977 | 0.023 | 1.10 |
| LCFR | 2 | 0.000 | 0.000 | 0.985 | 0.014 | 0.10 |
| LCFR | 3 | 0.000 | 0.000 | 0.962 | 0.038 | 0.05 |

- 轨迹平滑单调、**不崩尾**（uniform s1：bet1pot 0.81→0.96、allin 0.19→0.04 over 10M→100M）。两个平均模式、6 个 run 全收敛到「**基本不过牌、~98% 打满池、极小梭哈尾**」。
- **可剥削性 ≈0**（uniform −0.14~+0.32 BB；LCFR 0.05~1.10 BB），全在 4hJh ~2 BB（旧记 10 BB 系单位错，§12）之下。`EXPLOIT_CUR`（current 策略）11–20 BB = 正常 CFR 震荡 → averaging 健康、不饿死（ss_mass 21 万）。`EXPLOIT_STREET turn_only≈0 / river_only=full` 证实是单街河牌树。
- hero 范围值 ≈ **+14 BB**（uniform s1：u_hero=+1441 内部；nut-heavy 领打范围在 AAKK 面是 +EV）。
- **判定**：这个河牌子博弈**真解出来了**（收敛、跨 seed 一致、≈均衡）。**live 的 allin 是欠迭代/采样产物**——动作大类（下注 vs 过牌）正确，仅尺寸过梭（收敛后梭只占 1–4%）；live 的 0.69/0.31 ≈ 本 run ~10–15M 快照、非 100M 收敛值。

## 3. KdAc 河牌（真混合点）

**局面**：6-max 10/20，btn=seat2，hero=seat0（MP）。preflop UTG(5) fold → hero(MP) open45 → seat1(CO) call → btn/SB/BB fold；flop `6d3hTs` hero c-bet120（满池）→ CO call；turn `Td`（牌面成 `TT` 对子）hero check → CO check；river `5h`（`6d3hTsTd5h`）hero 先行动（轮起点）。`info_set 68068668931571808`，菜单 `[check,0.5pot,1pot,allin]`，有效栈 ~97BB（hero 1941 剩余）。
- **hero 牌力 = 最强的没成对牌**：AcKd 用牌面 TT + A、K 踢脚 = 实际 A 高；赢对手所有没成对的牌、输给任意一对 → give-up/诈唬两难。
- **来源**：从 vultr live 日志 `openpoker_actions_searchon1000_8s_par4.jsonl`（8s/par4 配置，最接近用户当前 run）扫出的「最混合 + 轮起点 + source=search」河牌点（用 `/tmp/find_mixed_river.py`，41 个候选里挑的 HU/~100BB/hero OOP 首行动点）。live 抽到 bet1pot。

**100M 收敛结果**（`check / half / pot / jam`；hero=子树玩家 4，`tree_seat(0)=(0+6−2)%6=4`）：

| 臂 | seed | check | half | pot | jam | hero v (BB) | 对手 v (BB) | expl σ̄ (BB) |
|--|--:|--:|--:|--:|--:|--:|--:|--:|
| uniform | 1 | 0.211 | 0.183 | 0.026 | **0.580** | −6.41 | +7.91 | 0.027 |
| uniform | 2 | 0.076 | 0.017 | 0.033 | **0.874** | −6.40 | +7.90 | 0.301 |
| uniform | 3 | 0.419 | 0.175 | 0.161 | **0.245** | −6.59 | +8.09 | 0.513 |
| LCFR | 1 | 0.479 | 0.031 | **0.447** | 0.043 | −6.41 | +7.91 | 0.035 |
| LCFR | 2 | 0.436 | 0.155 | 0.088 | **0.320** | −6.39 | +7.89 | 0.113 |
| LCFR | 3 | 0.273 | 0.082 | 0.192 | **0.452** | −6.58 | +8.08 | 0.688 |

- **策略跨 seed 完全不收敛**：梭哈频率横跨 **4%（LCFR-s1）→ 87%（uniform-s2）**；check 8%→48%；pot 3%→45%。三条轨迹方向各异（uniform-s1 check 升、s2 jam 升、s3 check 大升到 0.42；到 100M 多数还在漂）。**混合比例基本无法识别。**
- **但 hero 值钉死在 −6.4 ~ −6.6 BB（±0.1），可剥削性全 < 0.7 BB。** 6 个 run、两个平均模式、各种混合，值都一样。
- **常和校验**：`u_hero + u_villain ≡ +150 内部 = +1.5 BB`（= SB10+BB20 弃牌死钱 30 chip × 5），6 个 run 分毫不差。
- **判定**：这是**真·非唯一均衡**——河牌这子博弈有一大片**平的近均衡流形**，不同 seed 落在差异极大的点，全都几乎不可剥削、值都 ≈ −6.4 BB。

## 4. 结论

### 4.1 河牌子博弈会干净收敛（与 4hJh 转牌相反）

| | 街数 | infoset 数 | 河后未知牌 | 可剥削性 |
|--|--|--:|--|--|
| QsJc 河牌 | 1 | ~160 | 无 | ≈0 |
| KdAc 河牌 | 1 | 659 | 无 | <0.7 BB |
| 4hJh **转牌** | 2 | ~19,000 | 有(~44) | ~2 BB@500桶 → 1000桶腰斩（§12：单位修正 + 抽象受限非地板） |

机制：河牌是终局街，**根下没有发牌节点也没有下一街**。① 无发牌方差（转牌子树每迭代采 1/44 河牌、河牌 infoset 稀；河牌子树每迭代看到同一副完整牌面）；② 树小、每节点被访问得密（不饿死）。**→ 河牌是实时搜索最容易解的街，也不是 bot 离均衡问题所在；问题在上游（转牌/翻牌/翻前，下面压着发牌 + 额外下注轮）。**

### 4.2 两种「跨 seed 不一致」的本质区别（4hJh vs KdAc）

| | 跨 seed 策略 | 可剥削性 | 诊断 |
|--|--|--|--|
| 4hJh 转牌 | 不一致 | **~2 BB@500桶** | 远离均衡，但成因 = 500 桶抽象受限（1000 桶可降到 ~0.7、turn 解到 ≈0，§12），非迭代欠收敛 |
| KdAc 河牌 | 不一致（更夸张） | **< 0.7 BB** | 都**在**均衡上、非唯一（健康） |

**可剥削性是判据，频率不是。** 单看「跨 seed 频率分裂」会把这两种情况混为一谈（4hJh §5 E1 当初就栽在这）；加上可剥削性才分得开。

### 4.3 「值唯一、策略不唯一」的实证（两人零和可互换）

KdAc 6 个 run：策略横跨梭 4–87%，**hero v 全 ≈ −6.4 BB**。这正是 2p0s 的性质——**博弈值唯一，均衡策略集可非唯一**。推论：A 用解 1、B 用解 2 对打，hero 期望仍 ≈ −6.4 BB（可互换定理 / maximin 保证：任一均衡策略保证不低于 v，两边夹出 =v）。没直接测 cross-play head-to-head，但 6 个 u_hero 都 ≈ −6.4 → 已隐含。

### 4.4 v 怎么算

`v_i = (1/K) Σ_k u_i(σ*; deal_k)`，deal 从冻结的入口范围采（河牌牌面固定 → 只变双方底牌），`u_i` = 该 deal 下 σ* 走到终局时玩家 i 的**整手净筹码**。= 工具 `EXPLOIT_TURN detail` 里的 `u` 字段（`mc_profile_value`，独立 k_eval deal）。`可剥削性 = BR − u`，收敛时 `u ≈ BR ≈ v`。常和：`Σ_i u_i = 弃牌死钱`。注意 v 是**范围**值，非某单手。

### 4.5 「怎么打都可以」的边界

KdAc 看似「怎么打都行」（梭 25–87% 同 EV），准确说是**一大片平的无差异区**，原因 = 低杠杆点位（结果被入口范围决定、河牌这下动不了）+ 混合本就要等 EV + hero 无差异手牌数 > 对手约束数（欠定）。四条边界：
1. 是「同 EV」非分毫不差（还有 <0.7 BB 的浅梯度，seed3 expl 0.51/0.69 略高 = 流形上更偏的点）；
2. 不是「任何手任何动作」——严格被支配的打法仍抬 expl；
3. **只对完美对手成立**——实战对有漏洞对手这些混合不等价（钱靠针对性偏离赚）；
4. **此点位特有**——上游/高杠杆点位不平。潜台词：hero 在这本就是 −6.4 BB 烂局面，河牌救不回来。

### 4.6 注意 / 未坐实

- 单线程固定迭代，非 live 精确重放（§1）。
- 「非唯一均衡」是**抽象游戏**的；500 桶（桶内手牌同策略）+ 3 档尺寸离散会**额外制造**一些无差异 → 观测到的流形 = 真无差异 + 抽象无差异。expl≈0 只保证「在抽象游戏里这些混合都近最优」。
- 未直接测 cross-play（A=解1 vs B=解2 对打）head-to-head EV；由可互换 + 6 个 pinned u 间接得。
- QsJc 只取了 s1 的 u 详情（+14 BB），未跨 seed 列 u 表（其收敛性由策略 + expl 表已足）。

## 5. 复现

单 run（点位 req 见下，臂/seed 自选）：
```bash
SIX_MAX_TURN_TRACE_EVERY=10000000 SIX_MAX_EXPLOIT_KDEALS=8000 \
./target/release/openpoker_advisor \
  --checkpoint artifacts/run_6max_s4_preopen_n3_10b/nlhe_es_mccfr_final_010000000000.ckpt \
  --bucket-table artifacts/bucket_table_200_200_200_seed_cafebabe_schemav4.bin --reshape preopen \
  --search --search-trigger all-postflop --search-max-nodes 4000000 --search-deep-menu \
  --search-solve-threads 1 --search-bucket-table artifacts/bucket_table_default_500_500_500_seed_cafebabe_schemav4.bin \
  [--search-lcfr] --seed S --search-iterations 100000000 < <req.json> 2>err.txt
# TRACE_TURN 快照 + EXPLOIT_TURN/CUR/STREET/STREETSTATS 在 err.txt；最终 probs 在 stdout 末行
```
驱动脚本 `run_node_conv.sh <req.json> <outdir>`（6 run 串行）。解析 `parse_trace.py`。

vultr `~/dezhou_20260508/` 原始产物：
- `river_req.json`（QsJc）、`kdac_req.json`（KdAc，从 searchon1000 日志 line 1383 提取）；
- `river_qj_convergence/{uniform,lcfr}_s{1,2,3}.{out,err}`（QsJc 6 run）；
- `river_kdac_convergence/{uniform,lcfr}_s{1,2,3}.{out,err}`（KdAc 6 run）；
- 混合点搜寻：`/tmp/find_mixed_river.py`、`run_node_conv.sh`、`/tmp/parse_trace.py`。
- 工具分支 `tmp-subgame-exploit`（`9ce664f`）；Kuhn 验证 `cargo test --lib mc_exploitability_matches_exact_kuhn`。

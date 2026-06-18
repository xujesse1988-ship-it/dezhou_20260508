# 实时搜索转牌子博弈收敛性：Ac4c「坚果葫芦过牌 1.0」证伪（2026-06-18）

> 中性记录。是 `river_search_convergence_2026_06_17.md`（QsJc/KdAc **河牌**）的续作，同一套工具搬到一个**转牌**疑点上。
> 一句话定论：**转牌中坚果葫芦却 `check 1.0` —— 不是 bug，是真·均衡。** 跨 2 个桶数（500/1000）× 2 个平均模式（uniform/LCFR）× 3 seed = 12 个 run，hero 这个坚果桶的过牌频率全部收敛到 ≈1.0（单调爬升、迭代越多越过牌），可剥削性全 < 2 BB（1000 桶全 < 1 BB）。**桶变细（1000）反而 check 更死、可剥削性更低 → 排除"抽象造的假无差异"。** 机制：对手跟掉翻牌 check-raise 后，hero 整条 check-raise 范围其实是**劣势方**（hero 范围值 −1.4 BB），均衡让 hero **整个范围过牌**给对手、坚果跟着过牌（计划是 check-raise/check-call 而非领打）。

---

## 0. 起因

用户在 openpoker live 抽到一手，直觉「拿着坚果一路过牌太被动」：

- 6-max 10/20，btn=seat0，**hero=seat3（UTG）持 `Ac 4c`**。
- preflop hero open45 → seat4(MP) call → 其余全弃；单挑。
- **flop `4s 8h As`**：hero 两对（AA44）过牌 → 对手下 79（0.66 pot）→ **hero check-raise 到 357**（chosen raise1pot）→ 对手 call。
- **turn `Ad`**：牌面配对，hero 现在 **aces full of fours = 实质坚果**。`source=search` 输出 **`check 1.0`**（`info_set 151891277904871621`，solve_updates 7.1M）。← 疑点。
- **river `8c`**：牌面 AA88+4，hero **AAA88（aces full of eights，绝对坚果）**，又 `check 0.999`。

转牌→河牌拿着坚果连续过牌、一分价值不要，看着像被动 leak。本文只解这个**转牌根**（hero 首先行动 = 轮起点）；若转牌过牌成立，河牌过牌随之。

## 1. 配置与工具

分支 `tmp-subgame-exploit`（commit `9ce664f`）= 河牌/转牌文档同一套诊断工具：
- **`SIX_MAX_TURN_TRACE_EVERY=<N>`**：每 N updates 把**轮起点 + deep_menu** 决策点的平均策略打 stderr（`TRACE_TURN`）。本节点 = hero 转牌首行动 = 轮起点 → 触发；打的是 **hero 实际手牌（坚果葫芦）所在桶**在转牌根的平均策略。
- **`SIX_MAX_EXPLOIT_KDEALS=<K>`**：solve 完对 σ̄ 算 deal-积分 MC 可剥削性（`mc_exploitability`，Kuhn 精确单测吻合）。`EXPLOIT_TURN`（σ̄）+ `EXPLOIT_CUR`（current）+ `EXPLOIT_STREET`（按街拆 turn/river）。

参数（与 live 同 blueprint，差异 = 固定 100M 迭代 / 单线程 / 自选 seed / 无 prewarm，研究的是节点收敛性质非复现某次决策）：
```
--checkpoint artifacts/run_6max_s4_preopen_n3_10b/nlhe_es_mccfr_final_010000000000.ckpt  # 10B preopen
--bucket-table artifacts/bucket_table_200_200_200_seed_cafebabe_schemav4.bin --reshape preopen
--search --search-trigger all-postflop --search-max-nodes 4000000 --search-deep-menu
--search-solve-threads 1 --search-iterations 100000000
--search-bucket-table <500 或 1000 桶>     # 两遍都跑
[--search-lcfr]                            # uniform 臂关 / LCFR 臂开
--seed {1,2,3}
SIX_MAX_TURN_TRACE_EVERY=10000000 SIX_MAX_EXPLOIT_KDEALS=8000
```
设计：500 桶 6 run + 1000 桶 6 run，串行（每进程 ~7G RSS，11GiB box 一次一个），各 ~18 min。
**单位**：子树内部 chip = OpenPoker 5×（hero 全下内部 7990 = 真栈 1598×5）→ 1 BB = 100 内部 chip。

> 注：本仓已把"子博弈诊断默认只跑 1000 桶"定为默认（500 桶仅为与旧文档对照）。本次 500 桶保留是为了和 `river_search_convergence` 同口径横比 + 显式做抽象敏感性。

## 2. 转牌根收敛结果（hero 坚果葫芦桶：check / 0.5pot / 1pot / allin）

**500 桶**（infoset turn≈548 / river≈2390）：

| 臂 | seed | check@100M | expl σ̄ (BB) | turn_only / river_only (BB) |
|--|--:|--:|--:|--:|
| uniform | 1 | 0.9992 | 0.22 | −0.15 / +0.01 |
| uniform | 2 | 0.9992 | 0.70 | +0.02 / +0.15 |
| uniform | 3 | 0.9982 | −0.10 | −0.15 / −0.38 |
| LCFR | 1 | 1.000 | 1.70 | +1.86 / −0.16 |
| LCFR | 2 | 1.000 | 0.92 | −0.03 / +0.41 |
| LCFR | 3 | 0.9999 | 1.52 | +0.69 / −0.45 |

**1000 桶**（infoset turn≈581 / river≈3400，比 500 桶细 ~40%）：

| 臂 | seed | check@100M | expl σ̄ (BB) | turn_only (BB) |
|--|--:|--:|--:|--:|
| uniform | 1 | 0.9996 | 0.36 | +0.36 |
| uniform | 2 | 1.000 | 0.30 | −0.07 |
| uniform | 3 | 0.9999 | 0.55 | +0.36 |
| LCFR | 1 | 1.000 | 0.10 | +0.46 |
| LCFR | 2 | 1.000 | 0.89 | +0.34 |
| LCFR | 3 | 1.000 | 1.43 | +0.96 |

- **收敛轨迹单调爬向 1.0**（1000 桶 uniform_s1：10M 0.9963 → 50M 0.9992 → 100M 0.9996；下注三档全 → ~0）。**迭代越多越过牌** → 与"欠迭代导致过牌"完全相反（对比 QsJc：那手 live 的下注/梭哈是欠迭代产物，收敛后变下注）。
- **可剥削性低**：1000 桶全 < 1 BB（0.10–0.89）；500 桶 LCFR 两 seed 偏高（1.70/1.52）= **500 桶转牌街欠抽象**（`turn_only` 1.86/0.69 占大头），1000 桶腰斩到 < 1 BB —— 正是 4hJh §12 的桶敏感模式，但**策略（check 1.0）跨全部 12 run 不变**。
- **桶变细 → check 更死、expl 更低** → 排除"500 桶抽象压出的假无差异"。这是真均衡，不是抽象产物。

## 3. 为什么坚果葫芦过牌是均衡

**hero 范围值 ≈ −1.4 BB**（1000 uniform_s1：u_hero=−136.8 内部；u_villain=+286.8；常和 = +150 = +1.5 BB 死盲注，6 run 分毫不差）。即**对手跟掉翻牌 check-raise 之后，hero 整条 check-raise 范围在转牌反而是劣势方**（对手 call 把范围浓缩成强牌；hero 范围含半诈唬/听牌成分）。

机制：
1. **hero 范围不够强、领不动**：转牌 A 配对，hero 整条范围过牌给对手是均衡解（`EXPLOIT_STREETSTATS` 转牌街 mean_avg_entropy≈0.31、frac_near_uniform≈0.18 = 大多 infoset 近纯/低熵，符合"范围大面积过牌"）。坚果葫芦作为范围的一部分**跟着过牌**，计划 = 诱导对手领打后 check-raise / check-call，而非自己领打。
2. **过牌钓诈唬 > 下注赶空气**：对手跟掉 flop CR 后，到河牌一大把**破产听牌**（黑桃听牌被转牌 A、河牌 8c 全打废 = 空气）。对空气**下注会把它赶跑**；**过牌让它偷鸡**、hero 再抓 → 对坚果而言两条线 EV 打平（所以 100% 过牌是对均衡对手的最优应对，下注不多赚）。
3. **可剥削性 ≈0 坐实**：对手无法惩罚这条过牌线。

## 4. 结论

### 4.1 判决：`check 1.0` = 真·均衡，非欠迭代/抽象 bug
12 run（2 桶 × 2 平均模式 × 3 seed）一致 check ≈1.0、可剥削性 < 2 BB（1000 桶 < 1 BB）、单调爬升、桶变细更稳。**和 QsJc（live 下注是欠迭代产物、收敛后才"对"）正相反，这手 live 的 check 就是收敛值本身。**

### 4.2 "被动"的真相（= 河牌文档 §4.5 低杠杆平流形的同类）
hero 在这个点位**范围值 −1.4 BB、本就是劣势方**，转牌救不回来。坚果过牌是范围层面的最优、对**均衡对手**不漏 EV。但：
- **只对完美对手成立**。对**会 over-fold / 不偷鸡的真人弱对手**，过牌坚果是漏价值的 —— 该领打的价值要靠**针对性偏离**去赚（剥削，非 solver bug）。
- 这是 bot「解真游戏 → 结构性正确」与「实战榨弱对手」之间的固有缺口（见 `project_6max_realtime_search_goal_reframe`）。

### 4.3 注意 / 未坐实
- 单线程固定 100M、非 live 精确重放（live 12 线程墙钟、未知 base_seed、有 prewarm）。研究的是**节点收敛性质**。
- TRACE 打的是 hero **实际手牌桶**在转牌根的策略；未逐桶列 hero 全范围（由 `STREETSTATS` 低熵间接支持"范围大面积过牌"）。
- 河牌 `8c` 节点的过牌（0.999）未单独重解；由"转牌过牌成立 + 河牌是终局街最易解"推得，未直接验。

## 5. 复现

```bash
# req: ac4c_turn_req.json（turn 决策，hero 首行动）
SIX_MAX_TURN_TRACE_EVERY=10000000 SIX_MAX_EXPLOIT_KDEALS=8000 \
./target/release/openpoker_advisor \
  --checkpoint artifacts/run_6max_s4_preopen_n3_10b/nlhe_es_mccfr_final_010000000000.ckpt \
  --bucket-table artifacts/bucket_table_200_200_200_seed_cafebabe_schemav4.bin --reshape preopen \
  --search --search-trigger all-postflop --search-max-nodes 4000000 --search-deep-menu \
  --search-solve-threads 1 --search-bucket-table artifacts/bucket_table_default_1000_1000_1000_seed_cafebabe_schemav4.bin \
  [--search-lcfr] --seed S --search-iterations 100000000 < ac4c_turn_req.json 2>err.txt
```
vultr `~/dezhou_20260508/` 产物：`ac4c_turn_req.json`、`ac4c_river_req.json`、`run_turn_study.sh`、`ac4c_turn_conv_b500/`、`ac4c_turn_conv_b1000/`、`ac4c_turn_study.log`。

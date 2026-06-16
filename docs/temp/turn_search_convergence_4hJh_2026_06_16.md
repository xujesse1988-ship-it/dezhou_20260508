# 实时搜索单点收敛性诊断记录：turn 4h Jh on 8c5c6h9d（2026-06-16）

> 中性记录，供第三方评估。§1–§9 是调查过程（写于 2026-06-16 上午，**当时结论未定**）。**§10 是定论（2026-06-16 本次会话补）**：求解器无 code bug；解在这些预算下严重欠收敛、exploitability 随迭代不降（结构性地板），跨 seed 差异 = 两个都远离均衡的欠收敛点，**不是非唯一均衡**。先看 §10。

## 0. 起因

OpenPoker 实战一手，实时搜索在 turn 给出的策略疑似不稳。用户的理论判断：该子博弈实际是**两人零和**，CFR 平均策略应当收敛，且**无论什么随机种子（seed）结果应一致**；现观察到跨 seed 不一致，怀疑可能有代码问题。本记录用固定迭代 + 多 seed + 多配置复现并取证。

## 1. 测试点位

OpenPoker 原始请求（hero = my_seat 5 = BB）：

```json
{"hole":["4h","Jh"],"board":["8c","5c","6h","9d"],"button_seat":3,"my_seat":5,
 "num_seats":6,"small_blind":10,"big_blind":20,
 "actions":[{"seat":0,"action":"fold"},{"seat":1,"action":"fold"},{"seat":2,"action":"fold"},
            {"seat":3,"action":"raise","to":46},{"seat":4,"action":"fold"},
            {"seat":5,"action":"call"},{"seat":5,"action":"check"},{"seat":3,"action":"check"}],
 "valid":{"can_check":true,"can_call":false,"can_raise":true,"min_raise":20,"max_raise":2004},
 "stacks":[50319,2251,2895,2020,1020,2050]}
```

- 翻牌前：UTG/MP/CO（座 0/1/2）弃 → BTN（座 3）raise to 46 → SB（座 4）弃 → BB（座 5，hero）call。**只有座 3 与座 5 进池 = 两人（heads-up）局面。**
- flop `8c 5c 6h`：hero check、座 3 check。
- turn `9d`（board `8c5c6h9d`）：hero 首先行动，**本街尚无任何动作**。
- hero 牌力：4h Jh = J 高，没成手、没听顺（4 与 5-6-8-9 不连），基本空气。
- hand-start 真栈（OpenPoker 单位）：座 3 ≈ 2020、座 5 ≈ 2050（≈100BB）；turn 底池 ≈ 102 chip；有效栈 SPR ≈ 19。

**决策点 = 子树根**（turn round-start，within-round 无动作 → 导航落在根）。

**动作菜单**（deep_menu：SPR 19 ≤ 40×pot 且 Active ≤ 3 → 当前街放宽 `{0.5,1}`、下一街收回 `{1pot}`）：`{check, bet0.5pot, bet1pot, allin}`。allin 概率全程 ≈ 0（1e-6 ~ 1e-8 量级），下文表略去。

## 2. 配置与工具

生产/复现公共参数：

```
--checkpoint artifacts/run_6max_s4_preopen_n3_10b/nlhe_es_mccfr_final_010000000000.ckpt   # 10B preopen blueprint
--bucket-table artifacts/bucket_table_200_200_200_seed_cafebabe_schemav4.bin              # blueprint 桶表
--reshape preopen
--search --search-trigger all-postflop --search-max-nodes 4000000 --search-deep-menu
--search-bucket-table artifacts/bucket_table_default_500_500_500_seed_cafebabe_schemav4.bin  # 搜索桶表（默认 500）
--search-solve-threads 4        # 复现机 = vultr 4 核（用户 live 用 12）
[--search-lcfr]                 # LCFR 折扣开关
[--search-iterations N]         # 固定迭代（不给则配 --search-time-budget-ms 走墙钟）
[--seed S]
```

- **复现全部在 vultr（4 核 EPYC，11GiB 可用）跑固定迭代**，不用墙钟——固定 `(iterations, seed, threads)` 下结果确定性可复现（代码契约，见 §4）。
- **收敛轨迹仪表**：临时分支 `tmp-turn-trace`（commit `399443c`），给 `solve_subgame` 加 env 门控回调 `SIX_MAX_TURN_TRACE_EVERY=<updates>`，solve 过程中每 N 个 update 把**当前决策点平均策略**打到 stderr。
  - 提取逻辑与 advisor 最终输出**同源**（同一 `navigate_subtree` + `read_current_strategy` + `self_distribution`）。
  - **自校验**：`SIX_MAX_TURN_TRACE_EVERY=5e6 --search-iterations 1e7 --seed 0` 的 stdout 最终 probs（check 0.8035）与 trace@10M（0.80350）逐位一致。
  - env 未设时回调为 None，solve 循环逐位 byte-equal（clippy/fmt 干净）。
  - **注意**：该改动是临时诊断代码，未并入 6max，未做正确性长测。

## 3. live 原始读数（对照基准）

用户机器，墙钟 8s、12 线程、solve_updates 2,670,336：

| | check | bet0.5pot | bet1pot | allin |
|--|--|--|--|--|
| live 8s / 12 线程 | 0.5068 | 0.1035 | **0.3897** | 0 |

（墙钟 + 12 线程，与下文固定迭代 + 4 线程不直接可比；列此仅作起点。）

## 4. 算法事实（已核对代码）

- **求解器 = ES-MCCFR（External Sampling MCCFR）**。`solve_subgame` 建 `EsMccfrTrainer`，单线程走 `recurse_es`、多线程走 `recurse_es_parallel`（`trainer.rs:699 / :801`）：**chance sample-1**（`sample_discrete`）、**对手动作 sample-1**、**traverser 全枚举**。
  - 另有 `recurse_vanilla`（`trainer.rs:187`）是**全树 CFR**（chance/对手全枚举、零采样），但仅供另一个 trainer（Kuhn/Leduc 测试）使用，**子博弈解不走它**。
  - 下文“vanilla”一律指 **ES-MCCFR + 关 LCFR（均匀平均）**，**不是**全树 CFR。两条臂的采样都是 external sampling，差别只在平均加权。
- **LCFR period（本测试）**：不给 `--search-time-budget-ms` 时 `period = iterations / 50`（`subgame.rs lcfr_period`）。40M→period 800,000；100M→period 2,000,000。两者都恰好 **50 次 rescale**。period 绑总迭代数 → 同 update 数、不同总迭代的两次跑，rescale 时间表不同。
- **LCFR rescale 数学**（`trainer.rs:389 maybe_lcfr_rescale`）：period n 末乘 `n/(n+1)`，且默认同时作用于 regret + strategy_sum。累乘下来 period k 在第 N 期末的权重 = `k/(N+1) ∝ k`，即标准 Linear CFR（近因加权，period N 权重≈1、period 1≈0）。**核对为正确**。
- **strategy_sum 累积**：`S(I,a) += π_traverser × σ(I,a)`（`trainer.rs:248` 及 recurse_es 对应处），realization-weighted，**核对为正确**。`average_strategy` = strategy_sum 归一化。
- **deep_menu_for**（`nlhe_betting_tree.rs:323`）：≤3-way 且第二大 Active 栈 ≤ `40×pot` → 当前街 `{0.5,1}`、下游 `{1pot}`；本点位命中（SPR 19）。

## 5. 数据（全部固定迭代、4 线程、确定性可复现）

> 表内 `check / bet0.5pot / bet1pot`，allin 略（≈0）。

### 5.1 40M、LCFR、500 桶、5 seed（单 solve trace，period 800k，每 5M 一快照）

check 占比：

| updates | s0 | s1 | s2 | s3 | s4 |
|--:|--:|--:|--:|--:|--:|
| 5M  | 0.420 | 0.755 | 0.245 | 0.367 | 0.519 |
| 10M | 0.406 | 0.872 | 0.429 | 0.527 | 0.668 |
| 15M | 0.730 | 0.932 | 0.598 | 0.689 | 0.849 |
| 20M | 0.840 | 0.919 | 0.637 | 0.821 | 0.914 |
| 25M | 0.897 | 0.916 | 0.681 | 0.885 | 0.945 |
| 30M | 0.928 | 0.939 | 0.728 | 0.864 | 0.918 |
| 35M | 0.947 | 0.955 | 0.709 | 0.720 | 0.908 |
| 40M | 0.959 | 0.966 | 0.722 | 0.626 | 0.928 |

40M 完整分布：

| seed | check | bet0.5pot | bet1pot |
|--:|--:|--:|--:|
| 0 | 0.959 | 0.036 | 0.005 |
| 1 | 0.966 | 0.034 | 0.000 |
| 2 | 0.722 | 0.161 | 0.117 |
| 3 | 0.626 | 0.321 | 0.053 |
| 4 | 0.928 | 0.024 | 0.048 |

（s3 在 25M=0.885 后回落到 40M=0.626；s0/s1/s4 较单调。）

### 5.2 100M、LCFR、500 桶、seed 1/2/3（period 2M，每 10M 一快照）

| updates | s1 chk/½/pot | s2 chk/½/pot | s3 chk/½/pot |
|--:|--|--|--|
| 10M | 0.200 / 0.690 / 0.110 | 0.401 / 0.251 / 0.348 | 0.514 / 0.462 / 0.023 |
| 20M | 0.723 / 0.246 / 0.030 | 0.676 / 0.074 / 0.250 | 0.844 / 0.149 / 0.006 |
| 30M | 0.864 / 0.122 / 0.014 | 0.682 / 0.034 / 0.285 | 0.929 / 0.068 / 0.003 |
| 40M | 0.921 / 0.071 / 0.008 | 0.818 / 0.019 / 0.162 | 0.959 / 0.039 / 0.002 |
| 50M | 0.949 / 0.046 / 0.005 | 0.883 / 0.012 / 0.105 | 0.974 / 0.025 / 0.001 |
| 60M | 0.964 / 0.032 / 0.004 | 0.884 / 0.009 / 0.107 | 0.980 / 0.018 / 0.002 |
| 70M | 0.965 / 0.024 / 0.012 | 0.751 / 0.006 / 0.243 | 0.896 / 0.013 / 0.091 |
| 80M | 0.897 / 0.018 / 0.085 | 0.665 / 0.005 / 0.331 | 0.776 / 0.010 / 0.214 |
| 90M | 0.827 / 0.014 / 0.159 | 0.601 / 0.004 / 0.396 | 0.719 / 0.008 / 0.273 |
| 100M | 0.790 / 0.012 / 0.199 | 0.629 / 0.003 / 0.368 | 0.737 / 0.007 / 0.256 |

**全部 3 seed 都在 ~70M 之后尾部崩**：check 跌、bet1pot 飙。s1/s3 在 60–70M 曾到 0.96–0.98 再崩回 0.74–0.79。

### 5.3 100M、**关 LCFR**（vanilla / 均匀平均）、500 桶、seed 2/3（每 10M）

| updates | s2 chk/½/pot | s3 chk/½/pot |
|--:|--|--|
| 10M | 0.542 / 0.394 / 0.064 | 0.093 / 0.890 / 0.017 |
| 20M | 0.700 / 0.198 / 0.102 | 0.444 / 0.546 / 0.010 |
| 30M | 0.782 / 0.132 / 0.086 | 0.591 / 0.365 / 0.044 |
| 40M | 0.784 / 0.099 / 0.117 | 0.581 / 0.274 / 0.145 |
| 50M | 0.767 / 0.079 / 0.154 | 0.549 / 0.219 / 0.231 |
| 60M | 0.767 / 0.066 / 0.167 | 0.554 / 0.183 / 0.263 |
| 70M | 0.797 / 0.057 / 0.147 | 0.526 / 0.156 / 0.318 |
| 80M | 0.819 / 0.050 / 0.131 | 0.537 / 0.137 / 0.326 |
| 90M | 0.828 / 0.044 / 0.127 | 0.552 / 0.122 / 0.326 |
| 100M | **0.845 / 0.040 / 0.115** | **0.574 / 0.110 / 0.316** |

**均匀平均不崩、平稳**，但 seed2 收敛区（check≈0.84/pot≈0.12）与 seed3（check≈0.57/pot≈0.32）**明显不同**。尾段趋势：s2 check 仍缓升；s3 check 0.526→0.574、pot 0.326→0.316（刚出现轻微反向）。

### 5.4 100M、LCFR、**200 桶**（搜索桶表换 200/200/200）、seed 2（每 10M）

| updates | check / ½ / pot |
|--:|--|
| 10M | 0.651 / 0.266 / 0.083 |
| 30M | 0.495 / 0.433 / 0.073 |
| 50M | 0.594 / 0.263 / 0.143 |
| 70M | 0.441 / 0.341 / 0.218 |
| 90M | 0.308 / 0.310 / 0.382 |
| 100M | **0.252 / 0.289 / 0.459** |

**换 200 桶（更粗、每桶样本更多）在 LCFR 下没有变好，反而崩得最狠**（check 0.25 / pot 0.46）。

### 5.5 旁注：早期“分进程阶梯”（已被 §5.1 取代，含混淆）

最初用独立进程在 200k/1M/2.67M/5M/10M/20M/40M 各跑一次（seed 0）。因 `period = iterations/50`，**每个 level 的 LCFR 时间表都不同**（2.67M→period 53k vs 40M→period 800k），不是同一条轨迹，不可横比。仅留作记录：seed0 该网格 40M = check 0.96/½ 0.04/pot 0.005。

## 6. 观察（中性）

1. **LCFR 全部 seed 尾部崩**（§5.2）：先收敛到 check 0.96–0.98，再在尾段（占总迭代 ~30%）被一波 bet1pot 远足拽回 0.6–0.8。200 桶下更极端（§5.4）。
2. **均匀平均（关 LCFR）不崩、平稳**（§5.3）。
3. **同 update 数、不同 period，平均值差异巨大**：例 seed3 @40M，period 800k（§5.1）=0.626 vs period 2M（§5.2）=0.959。
4. **跨 seed 不一致在均匀平均下仍然存在**：seed2 ≈ 0.84/0.12 vs seed3 ≈ 0.57/0.32 @100M（§5.3），两者各自平稳但落在不同区。
5. allin 全程 ≈ 0；bet0.5pot 在长跑中普遍衰减。

## 7. 待解问题与互斥假设（**不下结论**）

核心未决：**两人零和下，平均策略应随 T→∞ 收敛到 seed 无关的同一策略；100M 仍未达到。** 这是“收敛慢”还是“有偏差/bug”？

- **H-A（收敛慢，非 bug）**：ES-MCCFR 每迭代只采 1 chance（河牌）+ 1 对手动作 → 单迭代方差大；4hJh 是 hero range 里的低频空气手，其 infoset 的有效样本（π_trav 加权）稀 → 平均估计收敛慢。
  - 支持：均匀平均每 seed 平稳、单调（§5.3）；LCFR/strategy_sum/采样代码核对为正确（§4）。
  - 未消除：均匀平均 seed2 0.84 vs seed3 0.57 @100M 差距仍大。
- **H-B（LCFR 近因加权放大）**：Linear CFR 权重 ∝ t，近期 period 主导平均；当前策略（regret-matching）在混合均衡附近震荡 + 采样噪声 → 近期的 bet1pot 远足被高权重计入 → 尾部崩。**与 H-A 不互斥**，是叠加效应。
  - 支持：均匀平均不崩、LCFR 崩（§5.2 vs §5.3），跨 500/200 桶都崩。
- **H-C（存在 per-seed 系统偏差 = bug）**：若再多迭代两 seed 仍不互相靠拢，则平均估计有偏（采样/归桶/reach 加权某处）。**未排除**。
  - 判别：均匀平均 seed2/seed3 跑到更高迭代（如 300M+），看 0.84/0.57 的 gap 收窄（→ H-A）还是卡住（→ H-C）。

其它可能相关：
- bet1pot ~0.10–0.15 在均匀平均 seed2 里稳定存在 → “空气在顺张面下注一部分”可能本就是均衡的平衡诈唬，并非纯噪声；但 seed 间 0.12 vs 0.32 的差异本身仍待解释。
- 全树 CFR（`recurse_vanilla`，零采样方差）能给确定性真值，但对全 range（1326×1326×46 河牌×下注序）不可行，未接入子博弈解。

## 8. 复现

请求文件 `/tmp/turn_req.json`（§1 的内层 req 一行）。单次（seed S、迭代 N、是否 LCFR、桶表）：

```bash
SIX_MAX_TURN_TRACE_EVERY=10000000 ./target/release/openpoker_advisor \
  --checkpoint artifacts/run_6max_s4_preopen_n3_10b/nlhe_es_mccfr_final_010000000000.ckpt \
  --bucket-table artifacts/bucket_table_200_200_200_seed_cafebabe_schemav4.bin \
  --reshape preopen \
  --search --search-trigger all-postflop --search-max-nodes 4000000 --search-deep-menu \
  --search-solve-threads 4 \
  --search-bucket-table artifacts/bucket_table_default_500_500_500_seed_cafebabe_schemav4.bin \
  [--search-lcfr] --seed S --search-iterations N < /tmp/turn_req.json 2>err.txt
# trace 在 err.txt 的 TRACE_TURN 行；最终 probs 在 stdout 末行
```

vultr 上的原始产物：
- `/tmp/turn_trace.tsv`（§5.1，40M×5seed）
- `/tmp/turn_trace_100m.tsv`（§5.2，100M LCFR×seed1/2/3）
- `/tmp/turn_disc.tsv` + `/tmp/disc_*_err.txt`（§5.3/5.4，vanilla + lcfr200）
- 代码：分支 `tmp-turn-trace`，commit `399443c`（origin 已推）。

## 9. 临时代码改动说明

`tmp-turn-trace`（399443c）仅对 `src/training/subgame.rs` 加了 env 门控的诊断 trace（§2）：
- `solve_subgame` 增加可选 `trace` 回调参；anchored 路径在 round-start + deep_menu 下构造闭包，复用既有提取函数读决策点平均策略。
- env 未设 → None → solve 循环 byte-equal；clippy/fmt 干净；本机 `cargo check` 通过。
- **未并入 6max；未跑相关 byte-equal 长测**。实验结束可删分支或合入（待定）。

## 10. 结论（2026-06-16 本次会话，定论）

一句话：**求解器无 code bug；这个双人子博弈在生产级预算下严重欠收敛，且 exploitability 随迭代不降（结构性地板）。跨 seed 不一致 = 两个都远离均衡的欠收敛点，不是「非唯一均衡」。**

### 10.1 代码审计：无 bug

19-agent 对抗审计 5 区域全部核为正确：
- **ES-MCCFR 估计器**（`trainer.rs:699-903`）：regret delta = `cfv − σ_value`（不乘 π_opp，对手/chance reach 由 external sampling 隐式带）、strategy_sum += π_trav·σ。逐行 vs `recurse_vanilla` 对比，无 reach 平方化/双计/泄漏。
- **regret matching**（`regret.rs`）：普通 RM（存带符号累积、只在取策略时 R⁺），平均归一正确，rescale 无溢出。
- **LCFR rescale**（`trainer.rs:389-404`）：50 period × `n/(n+1)` = 标准 Linear CFR，无 off-by-one/重复 rescale；尾崩是近因加权放大未收敛的 current strategy（averaging artifact），不是 bug。
- **root range / 2p0s**：range 一次冻结、self-play、4hJh 决策 = **子树根 → π_trav=1**（root 平均不饿死）。
- **并行 stale-σ**：delayed mini-batch、无偏、不移均衡集。

### 10.2 判决性工具：deal-积分 MC exploitability（新建 + Kuhn 钉死）

既有 `best_response.rs` 的 exploitability 只对 Kuhn/Leduc（in-tree chance 可枚举）可用。`SubgameNlheGame` 把整条 board（**含河牌**）+ 双方底牌一次性发在 `root()`、树内无 chance 节点 → 单次 BR walk = **河牌已知的「开天眼」最佳响应 = E[max]（带 Jensen gap 的上界）**，不是 exploitability。

新建 `mc_exploitability`（`src/training/best_response.rs`，branch `tmp-subgame-exploit`）：采 K 个 `root()`（= K 套底牌+河牌），把 per-(infoset,action) cfv 累进**同一张以桶为 key 的表** → policy iteration argmax（每桶提交单一动作、对发牌积分 = **非开天眼**）；BR 用 `k_train` 把发牌求、再在**独立** `k_eval` 把发牌上评估值（防过拟合 → 报值是**下界**，真值只会更高）。

`exploitability(σ) = Σ_i [ BR_i(σ_{-i}) − u_i(σ) ] ≥ 0`，用差值式（非 `(BR0+BR1)/2`），对含弃牌座 dead money 的**常和**也成立。**Kuhn 单测对既有精确 `exploitability::<KuhnGame,KuhnBestResponse>` 吻合 <1e-9 → 逻辑钉死后再用到子博弈。**

### 10.3 实测（vultr，单线程，uniform 平均，8000 train / 8000 eval deals；chip 单位，1BB=20，turn 底池≈102）

| seed | iters | 4hJh check | exploitability(sum) |
|--:|--:|--:|--:|
| 3 | 100M | 0.725 | **199.85** |
| 2 | 100M | 0.749 | **234.47** |
| 3 | 300M | 0.899 | **191.32** |
| 3 | 600M | （跑到 300M 中，待回填） | — |

**核心事实**：exploitability 卡在 **~190–235 chips（~5–6 BB/player）**。seed3 **同 seed、同 eval deals、100M→300M（3× 迭代）只降 4%**（199.85→191.32）——远非 √T 该有的 ~42% 降幅 = **结构性地板，加迭代基本不治**。同时 4hJh check 频率随 T 持续漂（0.725→0.899）但 exploitability 不动 = 策略在「等 exploitability 脊」上滑，没朝均衡走。

### 10.4 解释

- **跨 seed 不一致 = 两个都离均衡 ~5–6 BB 的欠收敛点**，不是两个合法纳什均衡。「非唯一均衡」是理论上正确的 caveat（2p0s 值唯一、策略集可非唯一；库恩 P1 的最优解就是一条连续族 α∈[0,1/3]），但对**本 spot 是误诊**——exploitability 把它否掉。
- exploitability **高度不对称**：一方 BR gain ~180 chips（占绝大部分），另一方 ~8–56。最可能主因 = 深层 infoset 的 **strategy_sum π_trav-加权饿死**（A3hh 机制：root π_trav=1 不饿、river 深层节点 reach 极小 → 平均冻在近 uniform → 被 BR 痛打）。**这类加迭代治不了，要换平均/求解方案。**
- **并行 stale-σ**：4 线程 §5.3 跨 seed gap 0.27（0.845/0.574）；单线程 100M 同节点 gap ~0.02（0.749/0.725），紧 ~13×。**但**两条轨迹都还在漂（seed3 100M=0.725→300M=0.899）→「单线程更紧」只是 100M 那一个快照上的巧合重合，**收回为弱证据**；稳的说法仅 = 同一 T 快照上单线程跨 seed 更紧，但都没收敛。

### 10.5 实际含义 + 修法方向

- **生产 8s / 12 线程实时搜索深在欠收敛区**：解出的策略对完美剥削者可被打 ~5–6 BB/手；单点频率（live 0.51/0.39 之类）基本是噪声抽样。
- **真修法（非调参；加迭代、换桶数都不治，200 桶已知更差）**：① 把子树根**重锚到决策点本身**（π_trav 全程 = 1，根除深层饿死，见 `project_6max_realtime_search_goal_reframe` §9.3 早记的方向）；② 换非 π_trav 的平均（vanilla / 链式 / 均匀 reach）。
- **下一步验证候选**：① seed3@600M 确认地板；② 按街拆 exploitability + 比 current vs average 平均 → 直接证死「深层饿死」是不是主因 → 定下换哪种平均。

### 10.6 复现

- 工具 + env：branch `tmp-subgame-exploit`（origin 已推；= `tmp-turn-trace` + `mc_exploitability` + `subgame.rs` 的 `SIX_MAX_EXPLOIT_KDEALS` 钩子）。命令同 §8，改 `--search-solve-threads 1`、去 `--search-lcfr`、设 `--search-iterations N`，并 export `SIX_MAX_EXPLOIT_KDEALS=8000`（可选 `SIX_MAX_EXPLOIT_KEVAL`）；exploitability 打在 stderr 的 `EXPLOIT_TURN` 行。
- Kuhn 逻辑验证：`cargo test --lib mc_exploitability_matches_exact_kuhn`。
- vultr 原始产物：`/tmp/B_expl_s2_100M.err`、`/tmp/B_expl_s3_300M.err`、`/tmp/B_expl_s3_600M.err`（含 `TRACE_TURN` + `EXPLOIT_TURN`）。

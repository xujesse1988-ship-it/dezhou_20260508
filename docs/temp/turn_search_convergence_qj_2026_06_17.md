# 实时搜索转牌子博弈收敛性 + 可剥削性诊断：QsJc（2026-06-17）

> 中性记录。是 `river_search_convergence_2026_06_17.md`（QsJc/KdAc **河牌**）的续作：**同一手 QsJc，往上挪一条街到转牌根**，验河牌文档 §4.1 的悬念——「河牌干净收敛、问题在上游（转牌/翻牌）」。
> 一句话定论：**QsJc 转牌子博弈，决策（hero 首动作 = 纯 check）、博弈值（hero −1.6 BB / BTN +2.6 BB）跨 6 run 全一致；LCFR 三 seed 把整树 σ̄ 干净收敛到 ≈0 可剥削；但 uniform 平均下 hero 侧残留 ~2–3.3 BB 真实可剥削（K=8000→40000 上修 + LCFR 双向夹死「是真残差、非噪声」），定性为「uniform 平均器对 hero 转牌策略的慢收敛」，不是 4hJh 转牌那种 ~10 BB 硬地板——本点位解得到 NE。**

---

## 0. 起因

接河牌文档 §4.1：河牌根下面没有发牌节点也没有下一街，所以干净收敛；问题应在上游。本文把同一套工具搬到 **QsJc 的转牌根**，看它会像它自己的河牌一样 ≈0 收敛，还是像 4hJh 转牌一样卡地板。

中途用户两次追问 ——「三个 seed 策略一样吗」「2 BB 不算噪声吗」—— 把实验从「单看收敛轨迹」推进到「**LCFR 臂对照 + K=40000 复核**」两道独立证据，本文据此定论。

**一处自我更正（已并入下表）**：初判时把 detail 的 player 0 当成 hero——错。座位→树玩家 = `(my_seat − btn_seat) mod 6 = (0−5) mod 6 = 1`，**hero = player 1，BTN = player 0**（与河牌文档 §2 `u_hero=+1441` 同源）。下文所有「按人」归因均已按 hero=P1 重算。

## 1. 配置与工具

分支 `tmp-subgame-exploit`（commit `9ce664f`），与河牌文档**同一套**工具，未并入 6max：
- **`SIX_MAX_TURN_TRACE_EVERY=<updates>`**：每 N update 把**子树根**（= round-start 决策点，`navigate_subtree(&[])`）的平均策略打到 stderr（`TRACE_TURN`）。末次快照 == advisor 正常输出 → 自校验。
- **`SIX_MAX_EXPLOIT_KDEALS=<K>`**：solve 完对 σ̄ 算 deal-积分 MC 可剥削性 `Σ_i[BR_i(σ̄_{-i}) − u_i(σ̄)]`，`k_train` deal 求 BR、**独立** `k_eval` deal 评估（去过拟合 → **报值是下界**，可微负，绕 0 抖）。同打 `EXPLOIT_CUR`（current 策略）、`EXPLOIT_STREET`（full/turn_only/river_only，BR 只许在该街偏离）、`EXPLOIT_STREETSTATS`（按街 infoset 数 / ss_mass / 熵）。Kuhn 精确单测吻合 <1e-9。

公共参数（沿用河牌 `run_node_conv.sh`，与 live 对齐差异同河牌文档 §1）：
```
--checkpoint artifacts/run_6max_s4_preopen_n3_10b/nlhe_es_mccfr_final_010000000000.ckpt   # 10B preopen blueprint
--bucket-table artifacts/bucket_table_200_200_200_seed_cafebabe_schemav4.bin --reshape preopen
--search --search-trigger all-postflop --search-max-nodes 4000000 --search-deep-menu
--search-solve-threads 1 --search-bucket-table artifacts/bucket_table_default_500_500_500_seed_cafebabe_schemav4.bin
[--search-lcfr] --seed S --search-iterations 100000000
SIX_MAX_TURN_TRACE_EVERY=10000000 SIX_MAX_EXPLOIT_KDEALS=8000
```
- **设计**：3 seed（1/2/3）× {uniform, LCFR} = 6 run，各 100M，每 10M 快照 + 8000/8000 deal 可剥削性。串行（每进程 ~7.0 GiB RSS，vultr 11 GiB 一次一个）。**每 run ~22 min，6 run ~2.2h。**
- **+ 噪声复核**：sweep 后对 uniform_s1（同 `--seed 1` 同 100M → **σ̄ 逐位确定性复现**）单独把 K 拉到 **40000**，隔离估计器方差。
- **与转牌（vs 河牌）的结构差异**：转牌根下压着**两层**——转牌下注 → 河牌发牌(44 张) → 河牌下注。这正是 4hJh 转牌卡地板的同类结构。
- **单位**：子树内部 chip = OpenPoker 5×，**1 BB = 100 内部 chip**（= 20 OpenPoker）。

## 2. 转牌节点 + 牌力

**局面**：6-max 10/20，btn=seat5，hero=seat0（SB）。preflop BTN(seat5) raise44 → hero(SB) call → BB fold；flop `Kd5cAs` hero bet108 → BTN call；turn `Kc`（牌面 `Kd 5c As Kc` = KK 对子 + A）hero **先行动**（轮起点）。`info_set 2951707684241507`，菜单 `[check,0.5pot,1pot,allin]`（SPR≈5 → deep_menu 给两档尺寸 + check + allin）。
- **座位映射**：hero = 子树 player **1**；BTN = player **0**。active=[0,1]（其余 preflop 弃）。
- **栈/池**：起始有效 ~89 BB（hero 1778 OpenPoker）；转牌 hero 剩 1626 = **8130 内部 = 81.3 BB**（AllIn to 8130 印证），pot 324 = **1620 内部 = 16.2 BB**，SPR≈5.0。
- **hero 牌力**：QsJc 在 `Kd5cAs Kc` = **QJ 高 + Broadway 卡顺**（任意 T 成 A-K-Q-J-T），无成手；paired KK + A 面。纯 check 合理。
- **live 决策**：`source=search`，check **0.985**（`solve_updates` 3,210,432）。

## 3. 6-run 结果

**转牌首动作 average 轨迹**（uniform_s1 为例，`check/half/pot/jam`；6 run 形态一致）：
```
 10M  0.991 / 0.004 / 0.005 / 0.000
 50M  0.995 / 0.001 / 0.004 / 0.000
100M  0.998 / 0.000 / 0.002 / 0.000   ← == advisor 输出（自校验）
```
单调、平滑、不崩尾。**6 run 末值全在 check 0.990–0.999** → 决策层 = ~纯 CHECK，跨 seed 跨臂一致。

**可剥削性总表**（单位 BB；BR 增益 = `BR−u`，负值=BR 没跑赢 σ̄=该侧≈0；hero=P1）：

| 臂 | seed | check | **HERO(P1) 增益** | BTN(P0) 增益 | full expl | turn_only | river_only | hero v | BTN v | 常和 |
|--|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| uniform | 1 | 0.998 | **+2.37** | +0.14 | +2.51 | +2.79 | −0.33 | −1.64 | +2.64 | +1.0 |
| uniform | 2 | 0.992 | **+2.32** | −0.38 | +1.94 | +2.12 | −0.05 | −1.12 | +2.12 | +1.0 |
| uniform | 3 | 0.990 | **+0.42** | −0.25 | +0.17 | −0.13 | −0.30 | −1.68 | +2.68 | +1.0 |
| LCFR | 1 | 0.998 | **−0.16** | +0.11 | −0.05 | −0.09 | −0.24 | −1.68 | +2.68 | +1.0 |
| LCFR | 2 | 0.999 | **+0.24** | −0.37 | −0.12 | +0.16 | −0.12 | −1.15 | +2.15 | +1.0 |
| LCFR | 3 | 0.998 | **−0.06** | −0.08 | −0.14 | −0.54 | −0.29 | −1.72 | +2.72 | +1.0 |

**K=40000 复核（uniform_s1，σ̄ 不变）**：

| | full | HERO(P1) 增益 | BTN(P0) 增益 | turn_only | river_only |
|--|--:|--:|--:|--:|--:|
| K=8000 | +2.51 | +2.37 | +0.14 | +2.79 | −0.33 |
| **K=40000** | **+3.25** | **+3.02** | +0.24 | +3.14 | **−0.01** |

**节点规模 / street stats**（确定性，跨 seed 近同）：转牌街(street2) ~960–989 infoset、ss_mass ~22k(LCFR)/45k(uniform)；河牌街(street3) ~5730–5853 infoset、ss_mass ~2.4k(LCFR)/4.9k(uniform)。总 ~6,700–6,800 infoset。**两街 ss_mass 都不接近 0 → 非 starvation。** 每 run 墙钟 ~22 min（K=40000 run 29:20）。

## 4. 结论

### 4.1 决策 + 博弈值：跨 6 run 钉死

- **决策** = 纯 CHECK（6 run 末值 0.990–0.999；live 0.985 正确）。
- **值** = hero −1.6 BB / BTN +2.6 BB（hero OOP、SB、led-flop 后被跟的弱 check 范围，转牌略 −EV 合理）。常和 `u_hero+u_BTN ≡ +1.0 BB`（= BB 弃牌死钱 20×5 = 100 内部）**6 run 精确**。
- **值的 ~±0.3 抖动是 eval-deal 采样噪声、非 σ̄ 差异**：seed-2（uniform & LCFR）hero v 都 ≈ −1.1（vs s1/s3 ≈ −1.7），两臂同 `--seed 2` → 同 eval deal → 同偏，证明是 deal 样本而非 σ̄。常和恒精确（per-deal 零和+死钱恒等式，MC 均值精确）。

### 4.2 可剥削性三态 + 「2 BB 是真的，不是噪声」

**LCFR 三 seed 全收敛到 ≈0**（full −0.05/−0.12/−0.14，hero & BTN 两侧都 ≈0）。这条同时锁死：
1. **估计器准**：同尺子（K=8000）量已收敛的 LCFR σ̄ = ≈0 ± 0.4 BB → 噪声底 ±0.4 BB 被独立坐实，没凭空造 2 BB。
2. **uniform 的 2 BB 因此是真的**：同节点 uniform-s1/s2 坐在 +2.3 BB ≫ ±0.4。反证：若是估计器噪声，LCFR 六测点也该散 ±2 BB，实测全 ±0.4 内。

**K=40000 从「估计器精度」独立再证**：σ̄ 逐位复现的前提下，full **2.51 → 3.25 BB**（涨）。该估计器去偏后是**下界**，deal 越多 BR 越准、下界越往上收；**噪声会随 K 缩向 0，它随 K 上修** → hero 侧真值 ≥3.3 BB，铁证非噪声。

→ **三态**：QsJc 河牌 ≈0（两臂）｜QsJc 转牌 uniform hero 侧 ~2–3.3 BB / LCFR ≈0｜4hJh 转牌卡 ~10 BB（两臂）。**可剥削性是判据，频率不是**（决策频率三态全是纯 check，分不开）。

### 4.3 残差落点：hero 侧、转牌街、根以外的决策

- **按人**：全在 **hero(P1)**（uniform s1/s2 hero +2.3、K40k +3.0；BTN 两臂全 ≈0）。
- **按街**：`turn_only ≈ full`、`river_only ≈ 0`，且 **K=40000 下 river_only 收紧到 −0.01**（5× deal 仍 ≈0）→ 河牌策略**真的近最优**，不是「测不出来」。K=8000 时 `full(2.51) < turn_only(2.79)` 的过拟合签名在 K=40000 消失（full 3.25 ≥ turn_only 3.14）。
- **根决策已收敛（纯 check）**，所以 2–3 BB 落在 **hero 的其它转牌 infoset**——最可能是 **hero check 后面对 BTN 下注的 call/fold/raise**（= live 同手第二个转牌点 `info_set 2965451579588707`，fold 0.72/call 0.28 的混合点）。

### 4.4 定性：uniform 平均器慢收敛，不是地板

- **LCFR 能把 hero 侧也收敛到 ≈0** → 子博弈**有 NE 且解得到**（彻底区别于 4hJh 转牌 ~10 BB 硬地板）。
- **uniform 慢**：给早期远离均衡的迭代等权，hero 那些跨范围的转牌混合应对洗得慢，100M 时 s1/s2 还剩 ~2–3.3 BB（s3 靠 seed 运气洗到 0.42）。LCFR 近期加权抹平。
- **机制旁证**：LCFR ss_mass 更低（street2 ~22k vs uniform ~45k）却收敛**更好** → 是**加权方式**起作用，不是采样量；两街 ss_mass 都不为 0 → 非 starvation。

### 4.5 对 bot 的意义

- **生产/live 用 `--search-lcfr`**，而 LCFR 把 hero 侧收敛到 ≈0 → **bot 实际用的臂没这个 2–3 BB 问题**。
- uniform 臂的 2–3 BB 是**对照警示**：在转牌（双街、hero 侧跨范围混合），**平均器选择对收敛很关键**，uniform 在 100M 预算下对 hero 转牌策略不够。
- 决策层（纯 check）两臂一致 → 单看「打什么」没区别；区别在「整树解到多干净」。

### 4.6 注意 / 未坐实

- 单线程固定迭代，非 live 精确重放（同河牌文档 §1）。
- 500 桶 + 3 档尺寸离散是**抽象内**可剥削性；BR 也在同抽象内 → 此 2–3 BB **不是抽象税**，是抽象内 σ̄ 离最优。但「真游戏」可剥削性另说。
- 未直接量 root 上 hero check 频率 vs hero 第二转牌点的 reach（哪个 infoset 主导 hero 侧残差是机制推 + EXPLOIT_STREET 间接证，未逐 infoset 拆）。
- LCFR「≈0」是 100M @ K=8000 读数；未对 LCFR 跑 K=40000 复核（uniform 已够说明问题）。

## 5. 对比表（接河牌文档 §4.1）

| | 街数 | infoset | 根下发牌 | LCFR expl | uniform expl |
|--|--|--:|--|--|--|
| QsJc 河牌 | 1 | ~160 | 无 | ≈0 | ≈0 |
| **QsJc 转牌** | 2 | ~6,700 | 有(44) | **≈0** | **2–3.3 BB（hero 侧, 慢收敛）** |
| 4hJh 转牌 | 2 | ~19,000(3-way) | 有 | 卡 ~10 BB | 卡 ~10 BB |

机制递进：河牌（终局街、根下无发牌）干净收敛；转牌（双街、根下压发牌+一轮下注）uniform 在 hero 跨范围混合侧慢收敛、LCFR 修得动；4hJh 转牌（3-way 深码不对称）连 LCFR 都卡 ~10 BB = 真欠收敛/地板。**QsJc 转牌坐在中间：可解到 NE，但对平均器/算力敏感。**

## 6. 复现

vultr `~/dezhou_20260508/` 原始产物：
- `turn_req.json`（QsJc 转牌轮起点，info_set 2951707684241507）；
- `turn_qj_convergence/{uniform,lcfr}_s{1,2,3}.{out,err}`（6 run）+ `uniform_s1_k40k.{out,err}`（K=40000 复核）；
- `run_node_conv.sh`（6 run 串行）、`run_k40k_after_sweep.sh`（SWEEP DONE 后自动 K=40000）、`/tmp/parse_trace.py`；
- 工具分支 `tmp-subgame-exploit`（`9ce664f`）；Kuhn 验证 `cargo test --lib mc_exploitability_matches_exact_kuhn`。

单 run：
```bash
SIX_MAX_TURN_TRACE_EVERY=10000000 SIX_MAX_EXPLOIT_KDEALS=8000 \
./target/release/openpoker_advisor \
  --checkpoint artifacts/run_6max_s4_preopen_n3_10b/nlhe_es_mccfr_final_010000000000.ckpt \
  --bucket-table artifacts/bucket_table_200_200_200_seed_cafebabe_schemav4.bin --reshape preopen \
  --search --search-trigger all-postflop --search-max-nodes 4000000 --search-deep-menu \
  --search-solve-threads 1 --search-bucket-table artifacts/bucket_table_default_500_500_500_seed_cafebabe_schemav4.bin \
  [--search-lcfr] --seed S --search-iterations 100000000 < turn_req.json 2>err.txt
```

# AIVAT 评测方法（Slumbot 单边对战降方差）

参考：Burch, Schmid, Moravčík, Bowling, *AIVAT: A New Variance Reduction Technique
for Agent Evaluation in Imperfect Information Games*, AAAI 2018 (arXiv:1612.06915)。

> 本文为 v2。v1 经 6 个角度的对抗 review + 逐条 skeptic 反驳后修订：修掉了 σ 还原、
> a\* 帧一致性、无偏性检验口径、runout 结算、−1969 锚点等真实问题；并澄清了若干被
> 误报为 critical（实际无偏）的点。带 **[v2 修订]** 的段落是相对 v1 的更正。

## 1. 目标与适用

- 收窄我方 blueprint vs Slumbot 的 `mbb/g` 95% CI / 减少所需手数。**无偏**。
- 预期幅度：单边设定 `P_a = {chance, 我方}`，论文 HUNL 自对弈 ~68.8% SD 缩减；我方值
  函数是自对弈代理（非真 Slumbot），实际预计 **2–3× SD 缩减（4–10× 更少手数）**。
  **点估计不变**，只收窄 CI。
- **[v2.1 实际资产]**
  - **数据（本机，8378 手）**：`slumbot_strategy_20260529_1.jsonl` 逐决策含 `info_set` +
    `action_probs`（4 位小数）+ `chosen` + `fallback_uniform` + `board_at_decision`；
    `slumbot_hands_20260529_1.jsonl` 含 `hole_cards`/`bot_hole_cards`（弃牌手也有）/`board`/
    `action`/`winnings`。→ σ 与 a\* **不必脆弱重放还原**：`a* = chosen`，σ 在 `info_set` 上
    全精度重算（见 §4.5）。两日志仅在本机，跑前 scp 到 vultr。
  - **blueprint（vultr）**：`artifacts/run_dense_lockfree/nlhe_es_mccfr_final_001000000000.ckpt`
    （1B ES-MCCFR dense，570MB）+ `artifacts/bucket_table_default_1000_1000_1000_seed_cafebabe_schemav4.bin`
    （**重训版，质量正常**，553MB）。VF 表必须用**这同一对** ckpt+表构建。
  - **该文件 raw 均值 ≈ +62 mbb/g**（**不是 −1969**；−1969 是另一次 1000 手 seed=7 跑，见
    `docs/status_v2.md`）。§7 闸门对**本文件自身** raw 均值配对比较，绝不锚外部常数。
  - **主机**：vultr（11 GiB / 4 core，已扩内存）即可建 VF + 跑估计器 + 校验，不需要 AWS。

## 2. 记号与单位

- chips 为整数；BB = 100 chips，200BB = 20000 chips。`mbb = chips × 10`（已对全 6447
  手核验 `mbb == winnings × 10`）。
- `U`：一手我方净收益（chips，我方视角）= `winnings`。
- 我方位置 `i ∈ {SB(button), BB}`。Slumbot `client_pos` 0 = BB / 1 = SB；我方 solver
  座位 = `1 − client_pos`（`slumbot_advisor.rs`）。VF 表、发牌/公共牌/动作修正一律按同一
  `i` 编址。
- **[v2 修订] `σ_I`：我方决策点实际抽样所用的分布**，即 advisor `decide()` 当时产生的
  `dist`：先按对战所用 `FallbackPolicy`（20260529 跑 = Hybrid，见 §8）取
  `row_sum_by_info>0 ? average_strategy : current_strategy`，**再叠加** decide() 的
  空行/全零→`uniform(legal)` 替换。**不是**笼统的"blueprint 平均策略"。
- 发牌均匀：我方 `C(52,2)=1326`；对方 `C(50,2)=1225`（扣我方 2 张）。
- **[v2 修订] 公共牌真实支撑**：发牌从一副去掉**双方 4 张底牌 + 已发 board** 的牌堆均匀
  抽。flop = `C(48,3)`，turn = `C(45,1)=45`，river = `C(44,1)=44`。

## 3. 估计量（控制变量形式）与无偏性

```
AIVAT(z) = U − Σ_{t ∈ T_known} c_t
```

`T_known` = { 我方发牌, 对方发牌, 每个已发公共牌事件, 每个我方决策点 }。
**Slumbot 的动作不在 `T_known` 内**。每个修正项：
```
c_t = V_t(实际孩子) − E_{x ~ p_t}[ V_t(孩子_t(x)) ]
```

**无偏性（核心）**：到达 t 父节点后，t 的结果按真实 `p_t` 抽取，故 `E[c_t | 父_t] = 0`；
由期望线性 `E[AIVAT] = E[U]` = 真值。对**任意**固定 `V_t` 成立，允许不同转移用不同 `V_t`。

**实现必须守的前提（违反即偏）：**
1. 单个 `c_t` 内部，realized 与所有 siblings 用**同一个** `V_t`。
2. sibling 集合与权重 = 该事件的**真实条件分布**（按已知牌去重后的剩余牌堆 / 我方 σ）。
   不可截断或无重加权采样。
3. **[v2 修订]** 我方动作修正的 `σ_I` 必须 = 对战时实际抽样所用 `dist`（§2 定义，含
   `FallbackPolicy` 分支 **和** 空行→uniform 替换层）。realized 抽象动作 a\* 必须是当时实
   际所选的那个（§4.5 帧一致还原）。
4. 跨修正项**不要求** `V_t` 互相一致。

> **[v2 澄清] 已被证伪、不必当 bug 的点**：公共牌 sibling 用 `C(48,3)`（扣双方底牌）还是
> `C(50,3)`（只扣我方）—— **两者都无偏**。组合恒等式：realized board 对随机对方底牌取边际
> 时恰好均匀于 `C(50,3)`（`C(47,2)/(C(50,2)·C(48,3)) = 1/C(50,3)`）。`C(48,3)` 只是更优的
> 控制变量（方差更低），故 §2/§4.3 选它，但这是 SE 取舍不是正确性。

## 4. 各修正项

记我方实际底牌 `c_us`、对方 `c_opp`、位置 `i`。`V_info`/`V_root_both` 见 §5。

### 4.1 我方发牌
```
c_deal_us = V₁(c_us, i) − (1/1326) · Σ_{c ∈ C(52,2)} V₁(c, i)
```
`V₁(c,i) = V_info[i, root, bucket = preflop169(c)]`。1326 项按牌对求和（每个 169 类多重度
自然计入）。

### 4.2 对方发牌
```
c_deal_opp = V₂(c_us, c_opp, i) − (1/1225) · Σ_{c'} V₂(c_us, c', i)
```
`c'` 取剩 50 张的全部对子；`V₂ = V_root_both`。`bot_hole_cards` 恒有 → 此项总可算。

### 4.3 公共牌（flop / turn / river），**该牌事件之后仍有决策**的分支
对每个已发公共牌事件 `b`：
```
c_b = V_info[i, node_b, bucket(c_us, board_after_b)]
      − (1/N_b) · Σ_{B} V_info[i, node_b, bucket(c_us, board_after_b(B))]
```
- **[v2 修订] sibling 集合 `B`**：扣掉 `c_us + c_opp + 已发 board` 后剩余牌堆的全部组合
  （flop `C(48,3)`，turn 45，river 44）。
- `node_b` = 发牌后下一 betting node；它由（player_acting, street, 抽象动作路径）决定，
  **与牌面无关** → 跨 siblings 不变，只 `bucket(c_us, ·)` 变（纯 re-bucket 循环，不重放）。
- `V_info` 按我方 bucket 编址（见 §5 keying note），即便 `node_b` 是对方行动节点也按
  `c_us` 取桶。
- **更强（可选）**：此处把 `V_t` 换成"`c_us` vs `c_opp` 在该 board 的精确两手 equity-EV"
  （条件在 `c_opp` 上）是最强的板面控制变量；opp-integrated `V_info` 也无偏，只是降方差弱。

### 4.4 全下 / 摊牌后纯发牌段（runout 收敛）
**[v2 重写]** 一旦双方再无任何决策（全下，或一方在某街后只剩发牌到摊牌），把这一段剩余
公共牌**合并为一项**：
```
c_runout = U_realized − E_runout[U]
```
- `U_realized` = 该手 `winnings`（真实落袋，net-PnL chips），**不得**用重放重算替代。
- **`E_runout[U]` 算法**：取该手**全下锁定时刻**的真实 `GameState`，对剩余 board 牌（从
  扣掉 `c_us+c_opp+已发 board` 的牌堆枚举所有补全）逐一：clone 该 state、把 `runout_board`
  的剩余张替换为该补全、跑 `finalize_terminal`、读 `payouts[us]`（整数 chips，net-PnL）。
  对所有补全取整数和再除以补全数。
  - 这样**自动复用** `return_uncalled_bets`（all-in-for-less 退注）+ main/side pot 划分 +
    平分 + `odd_chip_order`，与 `U_realized` 同一套结算口径，零漂移。
  - 等价闭式（匹配筹码 `m = min(committed_total)` 时）：`E_runout[U] = (2·eq − 1)·m`，
    `eq` = 精确两手 equity；但**实现走 clone+finalize**，不要写 `equity × pot()`（`pot()`
    含未跟注超额，all-in-for-less 下偏）。
- **`c_runout` 全手最多出现一次**，覆盖最后一个决策完成之后的全部剩余牌；它**替代** §4.3
  在这些牌事件上的修正（按"逐牌张事件之后是否还有决策"判定，不按街；不可重复扣）。

### 4.5 我方动作
我方每个决策点 `h`（info_set `I`，实际所选抽象动作 `a*`）：
```
c_act = V_child(a*) − Σ_a σ_I(a) · V_child(a)
```
- **[v2.1] `a*` 直接取 strategy log 的 `chosen`**（label，如 `call`/`check`/`fold`/
  `0.5pot`/`1pot`/`2pot`/`allin`），按 `action_probs` 的有序 key → 动作下标定位 tree child。
  彻底绕开"`outgoing_incr` vs `map_off_tree` 帧不一致"的还原坑。**禁止**重采样 σ 选 a\*。
- **[v2.1] `σ_I` 在日志 `info_set` 上全精度重算**（用同一 ckpt+表 + 对战 `FallbackPolicy`=
  Hybrid；`fallback_uniform=true` 的决策按 `uniform(legal)`）。日志 `action_probs` 是 4 位
  小数，**仅作 cross-check**（断言重算 σ 四舍五入后 == 日志值），**不**直接进加权和——否则
  舍入误差进入 `Σσ·V` 会给配对检验注入小偏差。
- **[v2 修订] `V_child(a)`**：`a` 的孩子是 betting node 时取 `V_info[i, child_node(a),
  bucket(c_us)]`（我方 bucket 不变）；`a` 的孩子是**终局**（弃牌 / 摊牌型 call/all-in）时取
  该终局对我方的**确定性 payoff**，而非 `V_info` 查表。
- 动作集合与顺序、`child(a)` 必须来自**同一份共享重放模块**的 `legal_actions(node)`；断言
  `len(σ) == len(legal) == len(children)`，`σ_I[k]` 对应 `children[k]`。

**[v2 修订] 合计**：
```
AIVAT = U − ( c_deal_us + c_deal_opp
              + Σ_{已发且其后仍有决策的牌事件} c_b
              + [若进入纯发牌段则 单一 c_runout]
              + Σ_{我方决策} c_act )
```
可修正的牌事件集合由**重放引擎判定的真实终局点**决定（不由最终 `GameState.board.len()`——
all-in 时被规则引擎补满、弃牌时不补——也不由动作串里的 `/` 计数）。弃牌手：无 `c_runout`，
`c_b` 只覆盖弃牌前已发且其后有决策的牌事件。

## 5. 值函数计算（blueprint 自对弈）

一次自对弈 Monte Carlo pass（blueprint vs blueprint，两位置各半，显式 `RngSource` +
`mix3` 派生 seed）。**注意：这是一个 `nlhe_eval.rs` 现在没有的新 builder**——现有
`rollout_blueprint_vs_baseline` 只返回终局 payoff，无逐节点累计。新 builder 同时产出：

- **VF-3 `V_info[i, node_id, bucket]`** = self-play `E[U | 我方 bucket, betting node, 位置]`
  （积掉对方 + 未来）。
  - **[v2 keying note]** 每条 rollout 走过的**每个 node**（含对方行动节点、我方动作之后的
    孩子节点），把该手对**评分座位**的最终 `U` 累计到 `(i, node_id, bucket(c_us, 当前
    board))`，键用 `SimplifiedNlheGame::info_set_for_cards(node, c_us, board)`——**不是**
    `Game::info_set`（后者按**行动方** bucket，在对方行动节点会变成对方的桶 → 错）。
  - 含 root → 同时给 VF-1（`V₁ = V_info[i, root, preflop169]`，带位置下标 `i`）。
  - **表规模**：行数 ≈ `NlheDenseIndexer::total_rows()`，`f64` 存储，**与 dense 策略表同量级
    （多 GB）**。在 AWS c6a.8xlarge（32–64 GB）建 + 评（评测器还要同时装 blueprint 算 σ）；
    **vultr 7.7 GB 装不下** blueprint + VF。
- **VF-2 `V_root_both[class_us, class_opp, i]`**：每条 rollout 在根记 `(我方169类, 对方169类,
  位置) → 最终 U`，均值。`169×169×2 ≈ 57k` 格，每格数百 rollout 即低噪。
- **VF-4**（§4.4 用）：不建表，按手 clone GameState + 枚举补全即时算。

VF 表为 `f64`（评测层，非 rules/abstraction 整数不变量路径，与 `nlhe_eval.rs` 同类，合规）。

> **MC 噪声不引偏**：sibling-average 直接用表值，realized 与 siblings 用同一张表 → `c_t`
> 仍精确均值零。噪声只略损降方差（见 §7 多 seed 校验）。

## 6. 实现

- 新 bin `tools/aivat_eval.rs`：加载 blueprint checkpoint + 桶表 + VF artifact + 数据日志。
- **[v2 硬要求] 共享重放模块**：把 advisor 的动作串解析 / abs+real lockstep 重放 /
  `outgoing_incr` / `legal_actions` 抽到 `src/` 共享模块，`slumbot_advisor` 与 `aivat_eval`
  共用同一份。a\* 还原、σ 计算、动作集合一律走它，杜绝两份漂移。
- **[v2.1] provenance 断言（运行前，不匹配即 abort）**：对加载的 ckpt 计
  `compute_strategy_blake3`，断言 == 本 blueprint（`run_dense_lockfree` 1B）实算值（首次跑
  时记录进 `docs/status_v2.md`，**不是**旧 100M 的 `2fab8a…`）；断言 bucket-table b3sum ==
  `bucket_table_default_1000_1000_1000_seed_cafebabe_schemav4.bin` 的值；`FallbackPolicy` =
  Hybrid。
- 输出：raw 与 AIVAT 并排的 `mbb/g`、SE、95% CI、按位置拆分，**外加配对差 `d` 的均值与
  CI**（§7）。
- **[v2.1] 主机**：日志（本机）scp 到 vultr；VF 构建 + 估计器 + 校验全在 vultr（11 GiB）跑。
  代码改动走 git push + vultr fetch/reset。

## 7. 正确性校验（全部在 vultr 跑，本机仅 build/fmt/clippy）

**[v2 重写] 无偏性主闸门用配对检验**（非配对"合并 CI"被 SE_raw 主导，太钝抓不到偏差）：
逐手算 `d(z) = AIVAT(z) − raw(z) = −Σ_t c_t(z)`，要求 `|mean(d)| ≤ 1.96 · SE(d)`，
`SE(d) = SD(d)/√N`。并**按修正类型**（deal_us / deal_opp / board / runout / act）分别报
`mean(c_t)` 与其 CI，把任何泄漏定位到具体项。

1. **Leduc 单测**：在 Leduc 上实现同一估计器，blueprint = 已知均衡，AIVAT 均值命中精确
   局值，`mean(d)` 不显著偏 0，SE ≪ raw。能让"错算法 fail"。
2. **自对弈 / 对已知 baseline 模拟（主闸门）**：模拟里每手同时算 raw 与 AIVAT，大 N 下
   `|mean(d)|` 落在配对 CI 内、`SE_AIVAT ≪ SE_raw`。模拟中对方牌恒知 → 覆盖"全量含对方
   发牌"。亦用于上线前预估 SD 缩减。
3. **真实日志一致性**：`|mean_AIVAT − mean_raw|` 落在**同一文件**配对 CI 内（该文件 raw 均
   值 ≈ +62 mbb/g，**不要**对任何外部常数如 −1969 锚定）；报告 SE 缩减比。
4. **VF 表噪声分量**：用 ≥3 个独立 self-play seed 重建 VF 表，确认各 seed 的 `mean_AIVAT`
   跨 seed 离散度 ≪ 手级 SE（CI 是"给定固定低噪 VF 的条件 CI"，需此验证其非主导）。

## 8. 不变量遵从

- 无浮点入 rules/evaluator/abstraction：AIVAT / VF 为评测层 `f64`，与 `nlhe_eval.rs` 同类，
  合规。`canonical_observation_id` / `BucketTable::lookup` 仍整数。
- 无全局 RNG：自对弈 pass 用显式 `RngSource` + `mix3` 派生 seed。
- §4.4 的筹码差走 `ChipAmount::checked_sub`（不变量 §4：下溢 panic）。
- off-tree 映射对**对方**下注不引偏（V 仅基线）；但**我方** a\* 还原必须帧一致（§4.5），
  否则选错 child → 偏。
- **[v2] σ 保真**：σ_I 必须复刻 advisor 的 `FallbackPolicy`（20260529 = Hybrid，advisor
  默认值）**和**空行→uniform 替换层；二者任一漏掉，对应决策的 `c_act` 非均值零。

## 9. 已知近似 / 未来增强

- §4.3 betting-continues 街默认用 opp-integrated `V_info`（无偏，降方差弱）；增强：条件在
  `c_opp` 的精确两手 equity（见 §4.3 可选）或双桶 postflop 值。
- preflop 用 169 类近似（与既有抽象一致）。
- perf：flop sibling `C(48,3)=17296` 次 `canonical_observation_id`+桶查表 / 手，仅多街非
  全下手需要（×约数千手 ≈ 1e8 次，分钟级）；turn 45 / river 44 平凡。非正确性。
- **最稳工作流**：今后对战一律开 strategy log，σ/a\* 逐字读回，彻底绕开 §4.5 还原与 §6
  provenance 断言的脆弱性。现有 6447 手日志按退路处理，并在报告里标注残余 provenance 风险。

## 10. 实现状态（2026-05-30 落地，含对前文的实测修订）

代码（branch `aivat-eval`，commit `eebb99b`+）：

- `src/training/nlhe_replay.rs`：advisor/estimator **共享**的 abs+real lockstep 重放（§6 硬要求）
  —— `tokenize` / `replay` / `resolve_actions` / `outgoing_incr` + 新增 `replay_trajectory`（整手
  轨迹：每决策节点 + 每街首决策 + 终局类型 + 各决策 real 快照）+ `tag_short_name`。
- `src/training/aivat_nlhe.rs`：估计器。值函数走 `AivatValueFn` trait（生产 `TableValueFn` 包 VF 表；
  测试注合成闭包，省多 GB 表 + blueprint）。
- `tools/aivat_eval.rs`：接日志 + VF + blueprint，provenance 断言 + Hybrid σ + 报告。

**两处修正本文前面的字面表述（正确性，已被 §7 闸门验证）：**

1. **§4.5 河前 all-in-call 的 V_child**：前文写"终局取确定性 payoff"。但河前 all-in-call 的孩子
   后面还有 runout chance，"realized payoff" 依赖未来随机 → **不是** child state 的固定函数 → 有偏 +
   留 runout 方差。正解 = `E_runout`（completions 平均，§4.4 同一量）。fold / 河牌摊牌（牌已全发）
   仍是确定性 payoff。
2. **§4.5 街切换 V_child（前文"我方 bucket 不变"未覆盖）**：关闭本街的动作（call/check 收口）孩子
   是**下一街**决策节点，下一街牌未发。不可偷看 realized board（否则非固定函数 → 有偏）。正解 = 对
   下一街新牌**积分** `avg_{新牌} V_info[child, bucket(us, board+新牌)]`。这恰让相邻修正项 telescoping
   （该积分 == 同 node_b 的 c_b sibling 平均），把 U 方差逐层剥掉。同街动作才"bucket 不变"。

**校验（全 vultr 跑）：**

- runout 交叉验证（`tests/aivat_nlhe_runout.rs`）：`runout_ev` 闭式 `m·(2eq−1)` 与 `showdown_net` ==
  GameState `compute_payouts` **逐 completion** 一致（river 44 / turn+river 990 / 含 all-in-for-less），
  3/3 过。
- 无偏主闸门（`tests/aivat_nlhe_selfplay.rs` #[ignore]，6000 手合成 VF+σ 自对弈）：
  `|mean(d)|/SE(d)=0.98`（无偏）；SE `143.17→76.52 = 1.87×`（≈3.5× 方差）即便 betting-node VF 合成
  ——降方差来自 runout/showdown 用真 equity。非常数 VF 激活所有 sibling 集合 → 抓 board 枚举 / 街切换 /
  位置 / telescoping 的结构 bug。

**实测修正 §1 资产事实：**

- 日志 `slumbot_strategy_20260529_1.jsonl` = **10000 手**（非 8378，run 续过）；**raw 均值 −85.25 mbb/g**
  （非 +62）；**0 个 fallback_uniform 决策**。client_pos 5001/4999。
- blueprint ckpt `run_dense_lockfree/...001000000000.ckpt` = **9.3 GB**（§1 写的 570MB 错；含 regret+
  strategy 两表）；bucket `..._1000_..._cafebabe_schemav4.bin` = **529 MB**（用户重训版，质量正常）。
- log `info_set` 解码（node = raw>>38 / bucket = raw&0xFFFFFF）实测：postflop bucket≤999、node≤239232、
  preflop bucket≤168 → **确认**该 log 由 1B dense + 1000 桶生成。

**主机/内存（修正 §1/§5/§6 的"vultr 11GiB 够"——错，按错的 570MB 估的）：** VF 表 total_rows≈2.39 亿，
mean+count ×2 位 ≈ 5.7 GB；**VF build = blueprint 9.3 + 累加器 5.7 ≈ 15 GB**、**eval = 9.3 + VF mean 3.8 ≈
13 GB**，都 > vultr 11 GiB（会进 swap，self-play 随机访问 9.3GB blueprint 会塌）。→ **生产跑要 ~32GB 机**
（AWS c6a.4xlarge ~$0.6/h）；§5 原判对，v2.1 乐观估错。ckpt 已在 vultr，跑前 vultr→AWS 传 9.3GB。

**生产跑结果（2026-05-30，AWS c6a.4xlarge / 真 1B blueprint + 真 10000 手日志 + 50M 自对弈 VF）：**

| 估计量 | mean (mbb/g) | SE | 95% CI | SE 缩减 |
|---|---:|---:|---|---:|
| raw | −85.25 | 174.71 | [−427.7, 257.2] | 1.00× |
| **AIVAT（推荐 = deals+runout）** | **−108.31** | **158.90** | **[−419.8, 203.1]** | **1.10×（方差 1.21×）** |
| AIVAT（full，含 board/act） | −172.35 | 175.03 | [−515.4, 170.7] | 0.998×（不降）|

- 两估计量配对差均无偏（rec d=−23.06 ± 145 / full d=−87.10 ± 275，均落 CI 内）；点估计差异是不同无偏
  估计量的抽样差，非偏差。两 CI 都跨 0 → 1 万手对 Slumbot 仍不显著，AIVAT 收窄 ~9% 半宽但不翻显著性。
- **降方差全来自精确/稠密项**：c_runout（all-in 精确 equity，单独 1.085×）+ 双方发牌（VF-1/2 覆盖好）。
  自对弈 VF 的 **board/act 修正净加噪声**（子集诊断：−deals−runout SE 158.90；再加 c_act→172.92 大幅变
  差、加 c_board→161.18 略差）——印证 §4.3/§9「opp-integrated V_info 降方差弱」，且 50M 手才 2.1% 行覆盖
  （`node_mean` fallback 让 c_board 从害变中性，治不了 c_act 桶盲噪声）。→ estimator 默认/推荐**只用
  deals+runout**（值函数可靠子集，仍无偏）；full 保留对照。
- VF build 50M 手 wall 238s（209k 手/s）/ eval 31s / RAM 峰 16GB。off-tree a* 6/10000 决策（a* 已用日志
  chosen 修正，replay map_off_tree 反推仅诊断）。
- **达 §1 预期 2–3× 的杠杆 = §4.3 精确两手 equity 控制变量**（日志含双方底牌 → equity 稠密 + 强相关 U）；
  flop sibling C(48,3)×completions 枚举昂（~8.5e10 eval7）→ 需 MC/粗粒度 flop equity 控成本，**未做**。

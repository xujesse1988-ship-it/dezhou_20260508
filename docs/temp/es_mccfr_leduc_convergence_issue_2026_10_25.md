# Leduc 上 ES-MCCFR 收敛偏慢 / 种子间波动大（已查明：平均策略累加位置错，2026-10-25）

来源：实现「实时搜索真实手牌单独信息集 + 强制发牌」时做的 Leduc 外部对照（`real_hand_forced_deal_design_2026_10_25.md` V1）。
那项对照按门槛通过了，但暴露出**基线本身**的问题，和强制发牌无关，单独记在这里。**只记录，没改代码。**

标注约定：**[事实]** = 实测或代码确认；**[推断]** = 有依据但没直接验证；**[推测]** = 只是怀疑。

> **调研结论（2026-10-25，详见 §7）**：§2 的怀疑**成立**。同一条训练轨迹上只把平均策略改到对手节点累加（权重 1），
> Leduc 2000 万次可剥削度从 0.150 ± 0.140 降到 **0.0037 ± 0.0003**；现行写法在 100 万次后基本停在 0.15–0.18 不再下降，
> 不只是慢，而是收敛到了错误的点。这个写法是 `240bb1a`（2026-05-17）引入的，之前是对的。
> 结论只直接适用于两人博弈；6-max（n ≥ 3）下「对手节点权重 1」本身也有偏差，见 §7.4。
>
> **进展（2026-10-26）**：多人实验（§9）表明 n ≥ 3 也该用「非遍历者节点、权重 1」。`trainer.rs` 全部人数和 dense 训练器已改（§9.5）；
> 实时搜索子树 `SubgameTrainer` 待改；blueprint 要重训。

## 1. 现象

Leduc 上用仓库自带的 `EsMccfrTrainer` 训练，用精确的 `LeducBestResponse` 算平均策略的可剥削度（`(br_p0 + br_p1) / 2`）。
5 个种子（0x11 / 0x22 / 0x33 / 0x44 / 0x55），单线程 `step` [事实]：

| 配置 | 更新次数 | 5 个种子的可剥削度 | 均值 ± 标准差 |
|---|---|---|---|
| vanilla ES-MCCFR | 200 万 | 0.127 / 0.294 / 0.047 / 0.179 / 0.216 | 0.172 ± 0.093 |
| vanilla ES-MCCFR | 2000 万 | 0.050 / 0.396 / 0.078 / 0.103 / 0.123 | 0.150 ± 0.140 |
| LCFR-MCCFR（period 40 万） | 2000 万 | 0.034 / 0.322 / 0.119 / 0.186 / 0.107 | 0.154 ± 0.108 |

（上表是「基线」一栏，即包装游戏 `q = 0`；`q = 0` 时与原版 `LeducGame` 训练逐位相同，测试 `forced_deal_q0_matches_plain_leduc` 钉住 [事实]。）

要点：

- 更新次数加 10 倍（200 万 → 2000 万），均值只从 0.172 降到 0.150，种子间离散度没有缩小 [事实]。
- 换成 LCFR 也一样 [事实]。
- 对照：同一个 Leduc 实现 + 同一个 best response，**vanilla CFR 1 万次迭代**就能 < 0.1（`tests/cfr_leduc.rs`，D-341 门槛）[事实]。
  所以 best response 和游戏实现本身大概率没问题 [推断]。
- 印象中外部采样 MCCFR 在 Leduc 上同等计算量能到 0.01 量级，这里差了一个数量级左右 [推测，需查原文核对数字]。
  按 `invariants.md` §8「偏离 10 倍以上要停下来怀疑算法」，值得查。

## 2. 怀疑点：平均策略的累加方式 [推测]

`recurse_es`（`src/training/trainer.rs` 约 773–779 行）在**遍历者自己的节点**上累加平均策略，权重是遍历者自身 reach：

```rust
if actor == traverser {
    // ... chance / opponent reach 由采样到该节点的概率隐式提供 ...
    let weighted: ShortVec<f64> = sigma.iter().map(|s| pi_trav * s).collect();
    strategy_sum.accumulate(info.clone(), &weighted);
```

平均策略的定义只按玩家**自身** reach 加权：`σ̄(I) = Σ_t π_i^t(I)·σ^t(I) / Σ_t π_i^t(I)`。

外部采样里，对手动作和机会都是采样的，所以第 t 次迭代走到 I 的频率 ∝ `π_{-i}^t(I)`（对手 + 机会 reach）。
在遍历者节点上再乘 `π_i^t(I)` 累加，期望上得到的是 `Σ_t π_{-i}^t(I)·π_i^t(I)·σ^t(I)`，多了一个 `π_{-i}^t(I)`。
如果它不随 t 变，按信息集归一后会抵消；但对手策略在训练中一直在变，`π_{-i}^t(I)` 跟着变 → 各次迭代的权重被扭曲，
偏向对手更常走到 I 的那些阶段 [推断]。这会拖慢收敛、放大种子间差异，但不一定改变最终极限 [推断]。

文献里外部采样的常见做法是在**对手节点**（非遍历者节点）上累加平均策略、权重 1：此时走到该节点的频率 ∝ 该节点玩家自身 reach × 机会 reach，
正好是定义里要的权重 [推断，需查原文核对；印象中 OpenSpiel 的 external sampling 的 simple 平均就是这么做的]。

代码注释写的是「在单条 sampled path 上再次乘 pi_opp 会把 reach 权重平方化」——说的是不能显式再乘 `π_opp`，这没错；
问题在于隐式带进来的那份 `π_opp` 本身就不该在平均策略的权重里 [推测]。

## 3. 影响范围（如果成立）

同一套累加方式在以下训练器里逐位照搬 [事实]：

| 位置 | 用途 |
|---|---|
| `src/training/trainer.rs` `recurse_es` / `recurse_es_parallel` | 通用 ES-MCCFR（Kuhn / Leduc / 测试） |
| `src/training/nlhe_dense_trainer.rs` 约 517、605 行 | **blueprint 训练**（dense 表） |
| `src/training/subgame_trainer.rs` 约 515、575 行 | **实时搜索子树求解** |

- regret 的更新不受影响，当前策略（regret matching）也不受影响；受影响的只有**平均策略** [推断]。
- 生产上 blueprint checkpoint 存的是平均策略，实时搜索读的也是子树的平均策略 → 如果成立，两处的实际决策都会受影响 [推断]。

## 4. 其它可能的解释（要先排除）

- ES-MCCFR 本身方差就大，2000 万次在 Leduc 上还不够 —— 和文献数字核对后才能判断。
- 可剥削度算的是平均策略；未访问的信息集按均匀处理（外部采样下 Leduc 信息集应都被访问到，可数一下）。
- LCFR 的 period 设置（40 万）是否合适。
- 单线程 `step` 与 `step_parallel` 是否表现一致（这次只测了 `step`）。

## 5. 验证计划

1. **加一个对照实现**：平均策略改在对手节点累加（权重 1），其余不动。Leduc 同种子、同更新次数比可剥削度曲线（200 万 / 2000 万，各 5 种子）。
   明显更低更稳 → 怀疑成立。
2. Kuhn 上同样对比（有解析解 `-1/18`，可直接看平均策略离均衡多远）。
3. 同时看最后一次迭代的当前策略 σ_T 的可剥削度，区分「平均方式的问题」和「采样方差」。
4. 和文献 / OpenSpiel 的外部采样实现核对平均策略的累加位置与权重。
5. 如果成立：评估对 blueprint（dense 训练）和实时搜索（子树求解）的影响，再决定怎么改。改训练器属于改算法，按 `invariants.md` §7 要有外部对照；
   blueprint 要重训，代价大，先在搜索子树上验证收益。

## 6. 复现

```bash
# 测试文件：tests/real_hand_forced_deal_leduc.rs（基线 = q 0）
LEDUC_FORCED_UPDATES=20000000 cargo test --release --test real_hand_forced_deal_leduc -- --ignored --nocapture
LEDUC_FORCED_UPDATES=20000000 LEDUC_FORCED_LCFR_PERIOD=400000 \
  cargo test --release --test real_hand_forced_deal_leduc -- --ignored --nocapture
```

AWS 8 核机器上每轮（2 种配置 × 5 种子 × 2000 万次）约 15 分钟。日志在该机 `~/rh_exp/logs/leduc_20m.log`、`leduc_20m_lcfr.log`。

## 7. 调研结果（2026-10-25）

### 7.1 对照实验 [事实]

探针：`examples/es_avg_probe.rs`（未提交）。自己写一份 ES-MCCFR，**同一条轨迹**（regret、当前策略、rng 流完全共用）上同时喂两张平均策略表：

- `trav`：遍历者节点、权重 = 遍历者自身 reach（现行 `recurse_es` 写法）；
- `opp`：非遍历者节点、权重 1（Lanctot 2009 / OpenSpiel 外部采样默认写法）。

`trav` 一栏与仓库 `EsMccfrTrainer` 逐位一致（30 万次 seed 0x11 都是 0.21622；2M / 20M 各种子数字与 §1 表完全相同），所以两栏的差别只来自累加方式。

Leduc，5 种子（0x11–0x55），可剥削度（`(br_p0 + br_p1) / 2`），均值 ± 标准差：

| 更新次数 | 现行 `trav` | `opp`（对手节点、权重 1） |
|---|---|---|
| 1 万 | 0.653 ± 0.100 | 0.242 ± 0.050 |
| 10 万 | 0.341 ± 0.080 | 0.0659 ± 0.0063 |
| 100 万 | 0.181 ± 0.138 | 0.0208 ± 0.0023 |
| 200 万 | 0.172 ± 0.093 | 0.0138 ± 0.0007 |
| 500 万 | 0.182 ± 0.176 | 0.0091 ± 0.0015 |
| 1000 万 | 0.173 ± 0.160 | 0.0061 ± 0.0006 |
| 2000 万 | 0.150 ± 0.140 | **0.0037 ± 0.0003** |

LCFR（period 40 万）同样结论：2000 万次 `trav` 0.154 ± 0.108，`opp` 0.0050 ± 0.0005（LCFR 在这里没有帮助，略差于 vanilla）。

- `opp` 随更新次数稳定下降（200 万 → 2000 万约降 3.7 倍，接近 `1/√t`），种子间几乎没有离散。
- `trav` 从 100 万次起基本平在 0.15–0.18，种子 0x22 一直在 0.3–0.5。**不是收敛慢，是停在了错误的点** [事实：平台；推断：极限本身有偏]。
- 最后一次迭代的当前策略可剥削度一直在 1.0–1.6（MCCFR 当前策略本来就不收敛），所以必须靠平均策略，平均方式错了就没有兜底。
- Kuhn（3 种子，200 万次）差别小：`trav` 0.0006、`opp` 0.0005。Kuhn 树太浅，对手 reach 随迭代变化的空间小，测不出这个问题。

### 7.2 为什么会停在错误的点 [推断]

§2 的推导是对的：遍历者节点上累加的期望是 `Σ_t π_{-i}^t(I)·π_i^t(I)·σ^t(I)`。MCCFR 的当前策略一直在振荡、不收敛，
`σ^t(I)` 和 `π_{-i}^t(I)` 是相关的（对手更常走到 I 的那些阶段，自己在 I 上的策略也系统性不同），
所以按 `π_{-i}^t` 加权的平均收敛到 `E[π_{-i}σ]/E[π_{-i}]`，而不是 `E[σ]`——偏差不会随迭代消失。
另外每个信息集的时间权重各不相同，拼出来的行为策略也不再对应「各轮策略的混合」，标准的「平均 regret → 可剥削度」界不再适用。

### 7.3 来历 [事实]

- `240bb1a`（2026-05-17「修正 Leduc ES-MCCFR 收敛口径」）之前，`recurse_es` 是在**对手节点** `strategy_sum.accumulate(info, &sigma)`，这是对的。
- 那次提交修了两个真 bug（终局 `u / π_trav`、regret 乘 `π_opp`），同时把平均策略挪到了遍历者节点。三处改动打包进一个提交，对照时没法分开看。
- 当时的外部对照是仓库根目录的 `leduc_mccfr.py`，它第 390 行也是在遍历者节点按 reach 累加（`info_set.accumulate_strategy(weight=reach)`）。
  参考实现和被测代码犯了同一个错，所以对照「通过」了。以后外部对照要找独立实现（OpenSpiel 等）。
- OpenSpiel `external_sampling_mccfr.py`：默认 `AverageType.SIMPLE` 在对手节点累加、权重 1，注释写明「两人时这就是标准实现（在对手节点更新平均策略）」，
  并说这与 Pluribus 做法一致；`AverageType.FULL` 则是另做一遍全树遍历、按自身 reach 加权。

### 7.4 对生产代码的影响

| 位置 | 人数 | 影响 |
|---|---|---|
| `trainer.rs` `recurse_es` / `recurse_es_parallel` | 2（Kuhn / Leduc / 测试） | 确认受影响，改回对手节点即可 |
| `subgame_trainer.rs`（实时搜索子树） | 子树内 Active 座数 | 读数点在子树根、hero 先行动时，根信息集前面没有对手决策（只有发牌），`π_{-i}` 是常数 → 根上读数**不受影响** [推断]；沿真实动作走到子树深处再读数则受影响 |
| `nlhe_dense_trainer.rs`（blueprint） | 6 | 除「前面没人做过决策」的信息集（如 UTG 首入）外都受影响 [推断]；**已训好的 checkpoint 的 strategy_sum 无法事后修正**，只能重训 |

**6-max 不能直接照抄两人的修法** [推断]：n ≥ 3 时，在玩家 j 的节点上权重 1 累加，走到该节点的频率里还带着其余非遍历者 k（k ≠ i, j）的 reach `π_k^t`，
同样随迭代变化，是同一类偏差（OpenSpiel 注释也只说两人时是标准实现）。多人下可选的做法：

1. Pluribus 式单独平均策略遍历：自己的节点按 σ 采样一个动作、计数 +1，其余玩家的节点全展开（Pluribus 只对翻前做、每 1 万次一次）[需核对原文]。无偏，但 6-max 全展开很贵。
2. 遍历者节点按 `π_i / π_{-i}(采样路径)` 加权（重要性修正）：无偏但对手 reach 很小时方差爆炸，不推荐。
3. 先在 3 人 Kuhn / Leduc 上量一下「对手节点权重 1」的偏差有多大，再决定 6-max 用哪种。

**和强制发牌（`2eee8b2`）的耦合**：改到对手节点累加后，hero 信息集的平均策略是在 hero **不**当遍历者时累加的；
强制发牌只在 hero 当遍历者时生效 → 不再增加 hero 真实手牌的平均策略样本；`min_hero_visits` 门槛数的也是遍历者访问次数，含义要跟着变。两件事要一起设计。

### 7.5 建议的下一步

1. ~~两人路径（`trainer.rs`）改回对手节点累加；Leduc 测试加一道新门槛~~ **已做（2026-10-25）**，见 §8。`leduc_mccfr.py` 还没改。
2. 子树求解器：先确认实盘读数点是不是总在子树根；如果是，现行写法对实盘决策影响有限，可以排在 blueprint 之后。
3. blueprint（6-max）：先做 §7.4 第 3 条的小博弈测量，再定修法；修完要重训，旧 checkpoint 作废。
4. 改训练器属于改算法（`invariants.md` §7），外部对照用 OpenSpiel，不用 `leduc_mccfr.py`。

复现（本机 2 核约 2 分钟一个 2000 万次）：

```bash
cargo build --release --example es_avg_probe
target/release/examples/es_avg_probe leduc 20000000 0x11          # vanilla
target/release/examples/es_avg_probe leduc 20000000 0x11 400000   # LCFR
target/release/examples/es_avg_probe check 300000 0x11            # 仓库训练器，应与 trav 栏逐位一致
```

## 8. 两人路径修正（2026-10-25）[事实]

`trainer.rs`：新增 `EsMccfrTrainer::average_at_opponent_nodes()`，**两人博弈且遍历者轮转覆盖两个座位**时，
`recurse_es` / `recurse_es_parallel` 在非遍历者节点按权重 1 累加平均策略；其余情况（n ≥ 3，或轮转跳过某个座位——那时
被跳过座位的对手永远不会被累加）保持旧写法，逐位不变。regret、当前策略、rng 消耗都不变。

- 新测试 `tests/es_mccfr_leduc.rs`：默认套件 10 万次 1 种子 < 0.1（实测 0.07034）；`--ignored` release 200 万次 5 种子各 < 0.03
  （实测 0.01390 / 0.01428 / 0.01458 / 0.01275 / 0.01348，与 §7.1 探针逐位一致）。
- `tests/real_hand_forced_deal_leduc.rs` 200 万次对照仍通过，而且灵敏得多：基线 0.0138 ± 0.0007，q = 0.5 为 0.0141 ± 0.0013（修前基线标准差 0.093）。
- 单元测试：`visit_counts_bookkeeping_only` 原来断言「有访问计数 ⟺ 有 strategy_sum」，两人时不再成立，改为断言 Kuhn 12 个信息集全部被计数；
  新增 `two_player_average_accumulates_at_opponent_nodes`。
- **已知副作用**（dense 部分已在 §9.5 消除）：HU（两人）配置下 HashMap 训练器与 dense 训练器不再逐位相同，`tests/dense_nlhe_trainer.rs` /
  `dense_nlhe_trainer_lockfree.rs` 的 HashMap 对照（`--ignored`，需 ≥ 8 GiB 机器）在 HU 下会失败。dense 是否同步改，跟 blueprint 一起定。同理，实时搜索的 `SubgameTrainer`（数组版，未改）与 `EsMccfrTrainer<SubgameNlheGame>` 的「逐位同结果」在 HU 子树下也不再成立；现有对照测试 `trainer_matches_hashmap_trainer` 用的是 6-max 子树，不受影响。
- **外部对照还没跑**：本机 `/tmp` 配额不够装 OpenSpiel。目前的依据是 OpenSpiel 源码（§7.3）+ 同轨迹探针，按 `invariants.md` §7 还欠一次实跑对照。

## 9. 多人（n ≥ 3）、dense、实时搜索子树怎么改（2026-10-25 调研）

### 9.1 实验方法 [事实]

探针 `examples/avg_multi_probe.rs`（未提交，只用标准库，自带 N 人 Kuhn / Leduc 和 N 人最优反应，不依赖仓库代码）。
多人下 CFR 不保证收敛到 Nash，可剥削度不能当判据，所以换基准：同一条外部采样轨迹上，每次更新后沿各玩家的
信息集父子链（完美回忆下 `π_p(I) = π_p(parent(I))·σ(parent(I), a)`）算出**精确平均** `Σ_t π_p^t(I)·σ^t(I)`，
再看各种采样估计离它多远（按精确 reach 加权的 L1 距离 `d`）以及各自的 NashConv（`Σ_i` 最优反应收益 − 当前收益）。

| 估计 | 位置 / 权重 |
|---|---|
| A | 遍历者节点，`π_i`（现行，n ≥ 3 时仓库仍是它） |
| B | 所有非遍历者节点，1（两人修正 `8da9a2b`） |
| C | 只在 `(i+1) mod n` 号玩家节点，1（OpenSpiel `SIMPLE`） |
| D | 遍历者节点，`π_i / Π_{k≠i} π_k(采样)`（无偏，重要性修正 A） |
| E | 玩家 j 的节点，`1 / Π_{k∉{i,j}} π_k(采样)`（无偏，只修正第三方；n = 2 时 = B） |

健全性：信息集数与仓库 / OpenSpiel 一致（两人 Kuhn 12、两人 Leduc 288、3 人 Kuhn 48）；两人 Leduc 30 万次 B 的可剥削度 0.033（§7.1 仓库探针 0.035）。

### 9.2 结果 [事实]

AWS 8 核机，均值（3–5 种子）。`nc` = NashConv，`d` = 离精确平均的距离。

| 游戏 | 更新次数 | 精确 nc | A nc / d | B nc / d | C nc / d | D nc / d | E nc / d |
|---|---|---|---|---|---|---|---|
| 两人 Leduc（1 种子） | 30 万 | 0.077 | 0.597 / 0.065 | 0.066 / 0.003 | = B | 0.426 / 0.051 | = B |
| 3 人 Kuhn | 300 万 | 0.0035 | 0.0203 / 0.0144 | 0.0036 / 0.0005 | 0.0091 / 0.0072 | 0.0143 / 0.0125 | 0.0037 / 0.0007 |
| 4 人 Kuhn | 300 万 | 0.0047 | 0.0291 / 0.0656 | 0.0047 / 0.0035 | 0.0257 / 0.0172 | 0.0347 / 0.0665 | 0.0049 / 0.0033 |
| 5 人 Kuhn | 300 万 | 0.0059 | 0.1127 / 0.1459 | 0.0060 / 0.0166 | 0.0540 / 0.0595 | 0.0852 / 0.1528 | 0.0058 / 0.0183 |
| 6 人 Kuhn | 100 万 | 0.0209 | 0.1462 / 0.1977 | 0.0162 / 0.0253 | 0.1002 / 0.1060 | 0.1462 / 0.2106 | 0.0179 / 0.0348 |
| 3 人 Leduc（3 点数，1 次加注） | 300 万 | 0.088 | 0.173 / 0.141 | 0.078 / 0.011 | 0.112 / 0.064 | 0.146 / 0.131 | 0.080 / 0.0095 |
| 3 人 Leduc（3 点数，2 次加注） | 100 万 | 0.327 | 1.463 / 0.212 | 0.280 / 0.034 | 0.933 / 0.093 | 1.414 / 0.200 | 0.308 / 0.028 |

- **A 在多人下同样坏**：`d` 停在 0.07–0.21 不降，NashConv 是精确平均的 2–7 倍，人越多越差。
- **B 在多人下仍接近精确平均**：NashConv 与精确平均在误差内相同（3 人 Leduc、6 人 Kuhn 甚至略低），`d` 随迭代下降；
  理论上 n ≥ 3 时 B 有偏（第三方 reach 隐式进权重），实测偏差小。
- **E 无偏但方差随人数增长**：3 人 Leduc 上 `d` 比 B 略小，5 人起反超 B（6 人 0.035 对 0.025）。9 人桌第三方 reach 连乘更小，权重更重尾 → 不用。
- C 样本少且同样有偏，D 方差爆炸，都不用。

**结论：所有人数统一用 B**（非遍历者节点、权重 1）。

### 9.3 各处改法

**(1) `trainer.rs` n ≥ 3、dense 训练器（blueprint）**：统一改成 B。`average_at_opponent_nodes()` 的条件从「两人且轮转覆盖两座」
改成「轮转里至少有 2 个不同座位」：只剩 1 个座位轮转时（如子树里对手全 all-in、只有 hero 能决策），该座之前没有任何对手决策，
A 恰好无偏，而 B 永远轮不到累加它。dense 三个递归（`recurse_es_dense` / `_parallel` / `_lockfree`）照同样改；
改后 HashMap ↔ dense 在 HU 和 6-max 下都重新逐位相同（`8da9a2b` 打破的 HU 对照恢复）。B 每次遍历的累加次数与 A 同量级（遍历者全展开的每个分支上，对手节点和遍历者节点交替出现），预计不会明显变慢，改完要实测吞吐。

**(2) 实时搜索子树（`SubgameTrainer`）**：分两部分，**必须一起改**。
- 各座位的分桶信息集：改成 B，同 (1)。它们会被用到：`cross_street_posterior_range` 用上一街子树的平均策略沿真实动作线算下一街各家范围。
- hero 真实手牌信息集（读数点）：不能只改成 B。B 下它只在 hero **不**当遍历者、且按范围恰好抽到真实手牌时才累加，样本极少，
  强制发牌（只在 hero 当遍历者时生效）帮不上；`min_hero_visits` 门槛数的也是遍历者访问。
  改法：**对真实手牌信息集直接累加精确平均**。`π_hero^t(I)` 只取决于 hero 自己沿公共路径的各真实手牌信息集的当前策略，
  沿子树里 hero 的决策节点做一遍「父节点乘到子节点」即可算出，与对手、采样都无关 → 无偏、没有采样方差，也和强制发牌不冲突
  （强制发牌继续只负责给 regret 多喂样本）。每 K 次更新扫一遍（K 取到额外开销约 10%；σ 每次更新只变一点，抽样间隔 ≪ 总迭代时与逐次累加几乎无差）。
  LCFR rescale 时同比缩放这张表。
- 读数点：当前实盘子树建在每条街开头（还有预热 / 缓存），hero 经常在对手行动之后、沿真实动作走到子树深处才读数
  （`navigate_subtree_by_real_actions`），所以现行 A 对实盘读数有实际影响 [推断，量级未测]。

**(3) blueprint 重训**：实盘用的 `nlhe_9max_flop2_1pot_cap4_b500_9b_preflop_flop_stratonly.crop.zst` 只存平均策略，无法修正。
regret 不受这个问题影响：如果全树带 regret 的 checkpoint 还在，可以从它接着训（strategy_sum 清零、用 B 再训一段），不必从零开始；否则只能重训。
新旧 blueprint 用 `tools/six_max_blueprint_h2h` 对打评估。

### 9.4 建议顺序

1. (1)：`trainer.rs` + dense 一起改成 B（机械改动，恢复两个后端逐位相同），3 人 Kuhn / Leduc 加门槛测试（对照精确平均或 NashConv）。**已完成，见 §9.5。**
2. (2)：`SubgameTrainer` 分桶信息集改 B + 真实手牌精确平均，一起提交；在 AWS 上用 `~/rh_exp` 的真实请求比较新旧读数分布，量一下实盘影响。
3. (3)：blueprint 重训（2026-10-26 定：不接着训，直接重训）+ 对打评估。

### 9.5 第 1 步实施结果（2026-10-26）[事实]

改动：
- `EsMccfrTrainer::average_at_opponent_nodes()` 的条件改成「轮转里至少 2 个不同座位」（不配轮转 = 全部座位轮转，恒为真），n ≥ 3 也走 B。
  只配 1 个座位（或同一座位重复）时保留 A。
- dense 三个递归（`recurse_es_dense` / `_parallel` / `_lockfree`）改成 B，不再需要的 `pi_trav` 参数删掉。
  顺带消除一条已知偏离：dense 的 touched 行现在等于访问过的全部信息集（遍历者节点写 regret、非遍历者节点写 strategy_sum），
  与 HashMap 的 key 集合相同；原来「只作为对手路过的信息集 dense 返回空、HashMap 返回均匀」不再出现（旧 checkpoint 里仍有这种行）。
- `SubgameTrainer` 没改（第 2 步）。它和 HashMap 训练器的逐位对照测试 `trainer_matches_hashmap_trainer`（3 人子树）暂时让 HashMap 一侧
  走旧写法（只在测试里可用的 `with_traverser_node_average()`），第 2 步改完删掉。

新测试 `tests/es_mccfr_multiplayer.rs`（自带 N 人 Kuhn / Leduc 和精确 NashConv，信息集数核对：3 人 Kuhn 48、两人 Leduc 288）。
「旧写法」一栏是同一测试临时强制走 A 实测的：

| 测试 | 新写法 | 旧写法 | 门槛 |
|---|---|---|---|
| 3 人 Kuhn 30 万次（默认套件，1 种子） | 0.0123（5 种子 0.0075–0.0153） | 5 种子 0.0076–0.0190 | < 0.03（粗门槛，不区分新旧） |
| 3 人 Kuhn 300 万次（`--ignored`，5 种子） | 0.0026–0.0068 | 0.0072–0.110，3 个种子 ≥ 0.01 | 每个种子 < 0.01 |
| 3 人 Leduc 3 点数 1 次加注 300 万次（`--ignored`，5 种子） | 均值 0.110（0.089–0.153） | 均值 0.180（0.134–0.231） | 均值 < 0.14 |

3 人 Leduc 新旧两组种子有重叠，所以门槛取均值。区分新旧主要靠单元测试 `three_player_average_accumulates_at_non_traverser_nodes`
（一次更新后遍历者自己的信息集不应出现在 strategy_sum 里）和两个 `--ignored` 测试。

回归（AWS 8 核，全部通过）：
- 默认套件里相关的：`training::trainer::tests` 4 个、`trainer_matches_hashmap_trainer`、subgame 里用 `EsMccfrTrainer` 的 3 个
  （`subgame_cfr_runs_and_is_deterministic` 等）、`nlhe_dense_crop` 3 个、`six_max_a3a4_trainer` 4 个、`es_mccfr_leduc`。
- `--ignored`：`tests/dense_nlhe_trainer.rs` 5 个 HU 逐位对照（`8da9a2b` 后不成立的那 5 个，现在重新通过）、
  `dense_nlhe_trainer_lockfree.rs` 4 个、`six_max_a3a4_dense_train_monitor`，以及新加的 `six_max_a3a4_dense_byte_equal_hashmap`
  （6-max 下 dense 与 HashMap 的 regret / strategy_sum 行、当前 / 平均策略逐位相同，touched 行数 = HashMap key 数）。

吞吐（`train_cfr` dense，6-max cap2 flop2-1pot，500 桶）：8 线程 lockfree 400 万次 旧 16.6 / 15.8 s、新 15.9 / 15.7 s；
单线程 50 万次 旧 9.11 / 8.93 s、新 8.88 / 8.94 s。没有变化。

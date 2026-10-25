# Leduc 上 ES-MCCFR 收敛偏慢 / 种子间波动大（待查，2026-10-25）

来源：实现「实时搜索真实手牌单独信息集 + 强制发牌」时做的 Leduc 外部对照（`real_hand_forced_deal_design_2026_10_25.md` V1）。
那项对照按门槛通过了，但暴露出**基线本身**的问题，和强制发牌无关，单独记在这里。**只记录，没改代码。**

标注约定：**[事实]** = 实测或代码确认；**[推断]** = 有依据但没直接验证；**[推测]** = 只是怀疑。

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

# 项目当前状态

本文档只记录"现在是什么"，不记录"我们做过什么"（后者查 `git log`）。
有变化就直接改这份文档。

## 算法正确性（最重要的一栏）

下表是核心算法在已知 ground truth 上的对照结果。
任何 stage 工作开始之前先看这栏。

| 项目 | 状态 | 依据 |
|---|---|---|
| Kuhn Vanilla CFR | ✅ 收敛到 closed-form Nash `-1/18` | `tests/cfr_kuhn.rs` |
| Leduc Vanilla CFR | ✅ exploitability `< 0.1` @ 10K iter | `tests/cfr_leduc.rs` |
| Leduc ES-MCCFR | ✅ 1M 外部对照通过；Rust 2M per-player update EV 与 `leduc_mccfr.py` 1M iter 同量级 | `leduc_mccfr.py` + `tools/leduc_es_mccfr_report.rs` |
| 简化 NLHE ES-MCCFR | ⚠️ 算法 pipeline + InfoSetId v2 layout + 1K/1M smoke 跑通；默认 profile = 200BB + 6-action {0.5p, 1p, 2p} + preflop lossless 169（postflop 走 500/500/500 bucket）；betting tree 实测 240,096 节点（18-bit）；blueprint 未训练（H4 范围） | `tests/cfr_simplified_nlhe.rs` + `tests/nlhe_infoset_history_collision.rs` + `src/training/nlhe_betting_tree.rs` + `src/abstraction/preflop.rs` |
| H3 简化 NLHE 闭环工具 | ✅ 训练入口、baseline 评测、LBR proxy、Markdown/JSON report、preflop strategy dump、抽象 betting tree sizing 工具齐备（200BB 重新跑数据待 H4） | `tools/train_cfr.rs` + `tools/nlhe_h3_report.rs` + `tools/nlhe_preflop_strategy_dump.rs` + `tools/nlhe_betting_tree_sizing.rs` + `tests/nlhe_h3_eval.rs` |

### 最近验证证据

- Leduc 外部参考：`python3 leduc_mccfr.py --iterations 1000000 --seed 7 --compact`
  输出 `Expected value for player 0: -0.08668`。
- Leduc Rust ES-MCCFR：`cargo run --release --bin leduc_es_mccfr_report -- --updates 2000000 --seed 0x4c454455435f4553 --report-every 2000000 --output artifacts/leduc_es_mccfr_2m_h2_status.txt`
  输出 `ev_p0=-0.087396516`、`exploitability_chips_per_game=0.258471407`、
  `average_strategy_blake3=b48a079c68fc595e722f3232e6c0219a52f91ebdabd1a9fadfe483ba9dce950a`。
  口径说明：`leduc_mccfr.py` 的 1 次 iteration 会分别更新两个 traverser；Rust `update_count`
  是 per-player update，因此用 Rust 2M update 对齐 Python 1M iter。
- Leduc 长跑趋势：已有 Rust 100M report
  `artifacts/leduc_es_mccfr_100m_full_history.txt` 输出 `ev_p0=-0.086036478`、
  `exploitability_chips_per_game=0.147556546`、
  `average_strategy_blake3=c0b8bcfa6db843b410f515b8526f08de19f573c88fb4eaf20afe431dba98385c`。
- bucket_table v3 artifact body BLAKE3 = `67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd`
  （与 stack profile 无关，仍可用作 H4 200BB 训练的输入抽象）。
- `tests/nlhe_infoset_history_collision.rs`：SB-aggressor vs BB-aggressor 两条 preflop 线推进到 flop
  同一决策点，InfoSetId 不同（按 node_id 区分）。
- 200BB + 6-action 默认切换后简化 NLHE 训练数据待 H4 重跑：LBR proxy 曲线、preflop 策略 spot 检查、
  fixed-seed snapshot BLAKE3 都需要在 200BB / 6-action 上重新 baseline。
  betting tree 节点数已实测：`tools/nlhe_betting_tree_sizing` 输出 240,096 决策节点（18-bit node_id），
  与 `NodeId = u32` + `InfoSetId v2` 26-bit cap 一致。

## 训练吞吐基线 + 200M run 实测（2026-05-20/21 实测）

### 吞吐基线（AWS c7i 32 vCPU EPYC 7R13 / 61 GiB）

`tools/train_cfr.rs` 在当前默认 200BB / 6-action / preflop 169 lossless / postflop 500-500-500 bucket
配置下，**200M update 实测衰减曲线**（5M sliding window 瞬时 throughput）：

| Update 区间 | inst throughput | 备注 |
|---|---|---|
| 0–5M | 10,679/s | warm-up |
| 5–10M | 11,084/s | **peak** |
| 10–20M | ~9,000/s | table 长大 |
| 20–50M | ~8,000/s | |
| 50–100M | 7,312/s | |
| 100–200M | **7,436/s** | 饱和稳态 |

整段 avg = 7,569/s。**衰减来源**：主 HashMap 长大后 cache miss 增加 + per-delta merge 操作数线性涨。
status 早期"32-thread steady 10,273/s"是 0-10M 窗口的数字，不是长 run 实测稳态。

### 200M run 总结

- compute (0 → 200M) = 26,066 s = **7.24 h**
- 100M auto ckpt = 6.59 GiB / 写盘 ~164s
- 200M final ckpt = 7.02 GiB / 写盘 358.7s
- **total wall = 7.34 h**（AWS c7i.4xlarge × 32 thread）

修订 wall 估算（按实测稳态 7,400/s）：

| 目标 | compute | 加 ckpt overhead | 总 wall | AWS cost @ $0.71/h |
|---|---|---|---|---|
| 500M | 18.8 h | +0.4 h | ~19.2 h | ~$14 |
| 1B | 37.5 h | +0.7 h | ~38 h | ~$27 |

### 瓶颈定位：`step_parallel` serial merge 是硬上限

`src/training/trainer.rs:367-430` 的 fork-join 形态：N 个 rayon worker 并行跑 DFS，主线程
**串行 merge N 个 `LocalRegretDelta` 回主 `RegretTable`**（`src/training/trainer.rs:420-427`）。
两组实测数据反推（早期窗口）：

- `T_game`（每 trajectory 并行成本） ≈ 296 μs
- `T_merge`（每 delta 串行合并成本） ≈ 88 μs（实测随 table 长大单调上涨）
- `throughput(N) = N / (T_game + N × T_merge)`，N → ∞ 时渐近 `1/T_merge ≈ 11,364/s`（早期峰值，
  长 run 实测因 cache miss / merge cost 增长降到 ~7,400/s）

**含义：加更多线程 / 更大机器都没用**，throughput 被 serial merge + 主表 cache miss 卡死。

### 已修：checkpoint 序列化排序键换成 `Ord::cmp`

旧路径 `entries.sort_by(|a, b| format!("{:?}", a.0).cmp(&format!("{:?}", b.0)))` 每次比较
分配 2 个 String，总 alloc 数 O(N log N)。500K updates 实测 RSS=10.7 GiB → final checkpoint
写盘 261 s（compute 才 148 s）。

修法：`Game::InfoSet` bound 加 `Ord`，`KuhnAction/KuhnHistory/KuhnInfoSet` /
`LeducAction/LeducStreet/LeducInfoSet` 全部 `derive(PartialOrd, Ord)`（`InfoSetId` 之前
就有），`encode_table` 改成 `a.0.cmp(&b.0)` 零分配排序。Kuhn 1000 × 10K BLAKE3 复现 ok /
Leduc 10 × 10K BLAKE3 复现 ok / Leduc ES-MCCFR 2M anchor `ev_p0=-0.087396516`
`exploitability=0.258471407` `blake3=b48a079c...` byte-equal。

副作用：bincode entry 顺序变（同 InfoSet 集合按 Ord 序 vs 旧 Debug 字符序）→
`tests/data/checkpoint-hashes-linux-x86_64.txt` 已刷新，`tests/cross_host_blake3.rs::
cross_host_baseline_byte_equal_for_current_arch` 由 fail 变 ok。

### 已尝试 + 回滚：`step_parallel` thread-local pre-aggregate（IndexMap）

`LocalRegretDelta` / `LocalStrategyDelta` 内部 `Vec<(I, SigmaVec)>` 换成
`IndexMap<I, SigmaVec>` 想做 DFS 内 in-place 累加 + dedup。预期 1.5-2.5× throughput。

AWS c7i 32 vCPU × 10M update 实测：

| 配置 | 0-5M steady throughput | wall（含 5M auto ckpt + final ckpt） |
|---|---|---|
| 旧 Vec append-only（status 原 baseline） | 10,273/s | n/a（500K 那次 RSS=10.7 GiB ckpt 261s） |
| IndexMap pre-aggregate（本次实测） | 9,285/s | 1077s compute + 125s 5M ckpt + 205s final ckpt = 1281.7s |

**结论：throughput 没提升反而略低**。根因：ES-MCCFR 一条 trajectory 在树状 DFS 下每个
traverser-actor InfoSet 只被访问一次（节点 `info_set = bucket | node_id | street_tag`，
不同 action 路径生成不同 node_id → 不同 InfoSet），IndexMap 的 dedup 没料可吃，
HashMap lookup 比 Vec push 多花常数。

已回滚到 Vec 路径。如果之后要解 serial-merge 瓶颈，必须碰跨线程合并顺序
（hash shard / lock-free atomic），那会破：
- Leduc ES-MCCFR 外部对照（Python `leduc_mccfr.py` 1M iter `-0.08668` anchor）
- Kuhn / Leduc fixed-seed BLAKE3 复现
- `cross_host_blake3` 跨 host byte-equal baseline

要不要做、做到什么程度，单独决策（不在本 step 范围）。

## 200M H3 报告（2026-05-21 实测）

ckpt：`artifacts/run_200m/nlhe_es_mccfr_final_000200000000.ckpt`（7.02 GiB / strategy_blake3
`ac87e9fcf5953cebb4658281bac8b0c89078b0bafca1c72df0baab9d3c51048a` / update_count 200,000,000）。

### LBR proxy 曲线（核心信号）

`tools/nlhe_h3_report --eval-hands-per-seat 1000 --lbr-probes 1000 --lbr-rollouts 16 --seed 0x42`：

| 阶段 | LBR BR chips | SE | probes | strategy hash |
|---|---:|---:|---:|---|
| uniform-0 | 5,617.85 | 212.56 | 912 | `uniform-empty` |
| 100M | **1,603.70** | 115.15 | 939 | `0d0d6b93...` |
| 200M | **1,614.96** | 114.03 | 952 | `ac87e9fc...` |

- uniform → 100M：**-71.5%**（CFR 在头 100M 学到大部分） 
- 100M → 200M：+11 chips 在 SE=115 噪音内，**多训 100M 没产出**

### Baseline EV（mbb/g，正值 = 训练侧赢）

| Baseline opp | mbb/g | 95% CI |
|---|---:|---|
| random | +8,674 | [6,090, 11,258] |
| equity-ev | +3,995 | [1,609, 6,381] |
| call-station | +2,705 | [1,878, 3,531] |
| overly-tight | +612 | [327, 896] |

四档都正显著（95% CI 全不过 0）。

### Preflop spot 检查（`tools/nlhe_preflop_strategy_dump`）

SB at root 关键 hand：

- AA：F=0 / C=0.097 / R500=0.337 / R1000=0.292 / R2000=0.273 / A=0（**不 AllIn**，premium 走 raise mix）
- AKs：F=0 / C=0.044 / R500=0.480 / R1000=0.437 / R2000=0.035 / A=0.003
- 72o：F=0.959 / C=0 / R500=0.037（96% fold trash，对）
- 22：F=0.007 / C=0.621 / R500=0.198 / R1000=0.174（limp/raise mix）

**没回归 100% AllIn**，preflop 形态符合 NLHE 直觉。

### by-street LBR slice（2026-05-21 实测，`--probe-filter has-average --lbr-probes 2000`）

| Street | LBR mean (chips) | SE | probes used | filtered | filter rate |
|---|---:|---:|---:|---:|---:|
| preflop | **1,640.2** | 84.2 | 1,905 / 2,000 | 0 | 0% |
| flop | 1,317.2 | 109.7 | 1,102 / 2,000 | 2,158 | 53% |
| turn | 1,321.4 | 138.1 | 687 / 2,000 | 3,521 | 64% |
| river | 1,268.5 | 172.0 | 464 / 2,000 | 4,342 | **68%** |

**两个反直觉的发现：**

1. **Preflop LBR 最高（1,640.2 chips），尽管 169-class lossless 无抽象损失**。filter rate=0
   说明 BR 估值干净，**preflop 残留 BR 跟 abstraction 无关，是训练样本质量问题**。

2. **Postflop 三街 filter rate 53/64/68%**：river 68% probes 落在 strategy_sum 全零的 InfoSet
   （200M update 期间 never visited）。ES-MCCFR sample-1 chance + sample-1 opponent action
   在 postflop 深处采样过浅，**不是 abstraction 到底了，是训练量不够覆盖**。

3. Postflop LBR 数字（1,268–1,321）**不可信**：filter 把"难"的 spot 全去掉了，剩下都是
   学过的 spot；effective probes（464–1,102）让 SE 跳到 110–172，统计噪音淹没差异。

## 下一步唯一允许的工作

### Step 3 重新决策：1B blueprint **可能值回票价**，收益在 postflop coverage

100M → 200M LBR proxy 飞机看似饱和（1,604 → 1,615 在 SE 内），但 by-street 数据揭示真相：
- preflop 已全覆盖但残留 1,640 chips BR → **更多训练数据有用**（不是抽象到顶）
- postflop 53–68% probes 没访问过 → **更多训练直接补 coverage**

1B blueprint 预算：~38h × $0.71 = ~$27。预期：
- postflop filter rate 显著下降（200M → 1B = 5× sample → river filter rate 可能从 68% → 30–40%）
- preflop LBR 可能继续降 200–500 chips（CFR 在 lossless preflop 上还没完全收敛）
- 总 LBR proxy 1,615 → 可能 1,200–1,400 量级

**操作顺序：**
1. 先 dump 200M ckpt 各街 InfoSet 访问数分布（如 `tools/nlhe_infoset_signal_dump` 或 ad-hoc），
   量化 postflop coverage 现状，给 1B 收益估值找一个先验。
2. 再决定：(a) 直接跑 1B；(b) 先做 abstraction 升级（K=500 → K=2000）；(c) 换 sampling
   形态（external sampling / outcome sampling 形态对 postflop coverage 的影响）。

### 不做（明确划界）

- **Parallel merge by hash shard / lock-free atomic 累加**：触碰跨线程 f64 顺序，
  Leduc 外部对照 + 跨 host BLAKE3 测试都要重做，ROI 不够（除非 H4 后还要反复训练大量轮次）
- Slumbot HTTP H2H 接入（H4 验收门槛但"不阻塞 first usable"）
- 500/500/500 postflop bucket 质量提升（stage 2 范围）

### 硬件备忘

vultr (4 core / 7.7 GiB) **跑不动** —— 第 3M updates 时 RSS 就到 7.6 GiB，进 swap
throughput 从 3,536/s 衰减到 808/s。1B 跑只能在 ≥ 16 vCPU / ≥ 32 GiB 的机器上。
本次用的是 AWS c7i.4xlarge（32 vCPU / 61 GiB，~$0.71/h），1B 完整跑成本 ~$20–25。

## 代码结构

```
src/
  rules/         规则引擎（座位 / 下注轮 / side pot / showdown）
  hand_eval/     7 张牌评估器
  abstraction/   action / information abstraction + bucket table
  training/      Game trait + Vanilla CFR + ES-MCCFR + checkpoint
tests/           cargo test 跑这里
tools/           一次性诊断 / 分析 binary
```

入口：

- `src/training/trainer.rs::recurse_es` — ES-MCCFR DFS（算法核心）
- `src/training/trainer.rs::recurse_vanilla` — Vanilla CFR DFS
- `src/training/leduc.rs` — Leduc 规则 + InfoSet 编码
- `tools/leduc_es_mccfr_report.rs` — Leduc ES-MCCFR 收敛报告
- `tools/train_cfr.rs` — H3 简化 NLHE ES-MCCFR 训练入口（checkpoint / resume / threads）
- `tools/nlhe_h3_report.rs` — H3 baseline 评测 + LBR proxy Markdown/JSON 报告

## 构建 / 测试

```bash
./scripts/setup-rust.sh                          # 一次性，装 rustup + 锁工具链
cargo build --all-targets
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test                                       # 默认套件，全绿
cargo test --release -- --ignored                # 长跑套件（含 BLAKE3 复现 / SLO）
```

可选 PokerKit 跨验证：

```bash
uv venv --python 3.11 .venv-pokerkit
uv pip install --python .venv-pokerkit/bin/python "pokerkit==0.4.14"
PATH=".venv-pokerkit/bin:$PATH" cargo test
```

## bucket table 工件

- `artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin`
  528 MiB / body BLAKE3 `67ee5554…98650cd` / 不进 git。

- 测试用 fixture（`--mode fixture --flop 10 --turn 10 --river 10`）每次现造。

## 已知污染清单

- `tests/bucket_quality.rs` 9 条 sqrt-scaled K=500 阈值断言 fail（v3 bucket_table，与 stack profile 无关）：
  - `adjacent_bucket_emd_above_threshold_{flop,turn,river}`：相邻 bucket EMD 实测 0.003 / 0.008 / 0.009，
    落到阈值 0.00894 以下（约 1.0–3.0× 偏差）。
  - `bucket_internal_ehs_std_dev_below_threshold_{flop,turn,river}`：bucket 内 EHS std dev 实测
    0.034 / 0.049 / 0.058，越上限 0.02236（约 1.5–2.6× 偏差）。
  - `bucket_id_ehs_median_monotonic_{flop,turn,river}`：bucket id 序与 EHS 中位数不单调，diff 实测
    0.010 / 0.017 / 0.011，超 MC-aware tol（约 1.2–1.7× 偏差）。
  - 在父 commit `ee4da88` 上同号同值复现，**非本次 200BB 切换引入**。
  - 根因：当前 500/500/500 bucket 抽象质量不达标，属 stage 2 重做范围
    （上文简化 NLHE 行 ⚠️ 状态的同一原因）。


## 文档维护规则

- 现在描述的状态错了 → **直接改本文件**，不追加"修订历史"。git 自带历史。
- 已经过时的事实 → 删除，不留"已废弃"标注。
- 新事实跟旧事实矛盾 → 旧的删掉，不并列。

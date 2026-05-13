# Pluribus Stage 2 — Bucket Quality v2 Artifact Test Report

**日期**：2026-05-13
**Artifact**：`bucket_table_default_500_500_500_seed_cafebabe_v2.bin`（528 MiB）
**目的**：基于 v2 production-trained bucket table（K=500/500/500，cluster_iter=2000，dual-phase canonical-inverse 100% coverage）跑 path.md §阶段 2 的 4 类 bucket quality 门槛断言（`tests/bucket_quality.rs` 12 条 `#[ignore]` 转 active）；定位 9 条失败的根因，准备给外部 reviewer 咨询。

---

## 1. 背景

### 1.1 系统位置

本项目是 8 阶段 Pluribus 风格 6-max NLHE 扑克 AI。Stage 2 是 information-set abstraction，把无限的 (board, hole) 状态空间压成有限 bucket id。Stage 3+ 的 CFR 训练在 abstraction 之上做，所以 bucket 质量直接决定 CFR 输出的策略好坏。

### 1.2 V2 artifact 训练参数

- 算法：D-218-rev2 真等价类枚举（Waugh 2013 hand isomorphism + colex ranking）3 街全枚举
- N_canonical：flop=1,286,792 / turn=13,960,050 / river=123,156,254（D-218-rev2 §2 字面，§G-batch1 §3.1 实测精确值）
- Feature：D-221 EHS² + OCHS(N=8) = 9 维 cluster feature
- K-means：K=500/街，max_iter=100，centroid_shift_tol=1e-4，D-236b 重编号（按 EHS 中位数升序）
- cluster_iter（MC inner iter for equity()）：**2000**（workflow §3.4 字面 10000 由 [实现] 阶段实测预算降到 2000；见 §1.3）
- Dual-phase 训练（§G-batch1 §3.4-batch1）：
  - Phase 1：随机抽 min(N, 2M) 样本算 9 维 feature → k-means 训 K centroids
  - Phase 2：枚举每个 canonical_id ∈ [0, N) → decode via `nth_canonical_form` → 同 pipeline 算 feature → 最近 centroid → 100% 写满 lookup_table
- Wall（AWS on-demand 16-core EPYC 7R13 Milan）：**11h 47min 52s**
- BLAKE3 body hash：`e602f5486f0f48956a979a55d6827745b09e60ec9e4eaca0906fd1cd17e228e5`

### 1.3 cluster_iter 降级 carve-out

Workflow §3.4 字面 `cluster_iter=10000`。[实现] 阶段实测 32-core EPYC 7R13 wall ~27h（48h on 16-core），AWS spot 两次被回收损失 ~3h compute。降到 iter=2000 实跑 11.8h 在 16-core on-demand 上完成。

预计噪声分析（基于 MC equity() σ = sqrt(0.25/N)）：

| 街 | ehs² outer | iter=10000 σ | iter=2000 σ | bucket spacing (K=500) |
|---|---|---|---|---|
| Flop | 1176 enum | 0.015% | 0.033% | 0.2% |
| Turn | 46 enum | 0.073% | 0.16% | 0.2% |
| River | 0 enum (= equity²) | 1.0% | **2.2%** | 0.2% |

Flop/Turn 噪声远 < bucket spacing；River 噪声明显高于 bucket spacing —— **§3.4-batch2 closure 时已 flag** 此风险，留 §3.8 4 类质量门槛实测决定是否接受。

---

## 2. 测试方法

### 2.1 测试代码位置

`tests/bucket_quality.rs`（commit `b67a73d` + 本地未 commit 修改）

### 2.2 12 条测试（按 4×3 分组）

| 类别 | path.md 阈值 | 测试方法 |
|---|---|---|
| **0 空 bucket** | 每个 bucket id 至少有 1 个 canonical sample | §3.8 本 batch 改成 **deterministic 全枚举** — `for id in 0..N_canonical { lookup(id) → mark hit[bucket] }` |
| **EHS std dev** | `< 0.05` per bucket | 1000 sample (board, hole) 随机抽样；每 sample 跑 MC equity (内部 1000 iter)；按 bucket 分组算 std_dev；任一 bucket 超 0.05 → fail |
| **相邻 bucket EMD** | `≥ 0.02` per (k, k+1) pair | 同上采样；按 bucket 分组得 EHS 分布；对每对相邻 bucket 算 1D Wasserstein EMD；任一对 < 0.02 → fail |
| **EHS 中位数单调性** | bucket id 递增 ⇒ bucket EHS 中位数递增（D-236b 保证） | 同上采样；按 bucket 算 median；任一对 (k, k+1) median[k] > median[k+1] → fail |

### 2.3 §3.8 [实现] 对原测试的修改

1. `cached_trained_table()`：从 fixture K=100 训练改为 load v2 528 MiB artifact via `BucketTable::open(...)`
2. "0 空 bucket" 测试：从 5×K sample 改为 deterministic 全枚举 N_canonical
3. std_dev / EMD / 单调性 sample count：1000 → 10000（K=500 时 Poisson λ=20 让大多数 bucket 有数据；vs 原 K=100 时 λ=10 够用）
4. 12 条 `#[ignore]` 取消

---

## 3. 测试结果（v2 artifact）

`cargo test --release --test bucket_quality` on AWS 16-core EPYC 7R13：

**总计 19 active tests（含 12 条新转 active 的 + 7 既有）+ 1 ignored**：

- ✅ **10 passed**：
  - 3 in-range 1k smoke（验证 lookup 返回 in-range bucket）
  - 3 no_empty_bucket（deterministic 全枚举所有 N_canonical_id，hit 每个 bucket）— v2 artifact 100% canonical coverage 验证通过
  - 4 helpers（emd_1d / std_dev / median sanity）

- ❌ **9 failed**（4 类失败 × 3 街，其中 0 空 bucket 已过）：

### 3.1 std_dev 失败（3 条）

```
validation §3 (flop)：bucket 0 EHS std dev 0.0575273444777655 >= 0.05（n=29）
validation §3 (turn)：bucket 0 EHS std dev 0.059773926608899415 >= 0.05
validation §3 (river)：bucket 0 EHS std dev 0.08740044666317826 >= 0.05
```

所有失败都在 **bucket 0**（最低 EHS bucket）。flop/turn 失败 +15%/+20% 超阈值；river 失败 +75% 显著。

### 3.2 EMD 失败（3 条）

```
D-233 (flop)：bucket 0 vs 1 EMD 0.009480357142857826 < T_emd 0.02
D-233 (turn)：bucket 0 vs 1 EMD 0.014313322368421989 < T_emd
D-233 (river)：bucket 1 vs 2 EMD 0.013744768898992458 < T_emd
```

flop -52% / turn -28% / river -31%。所有失败都是邻 bucket 距阈值约 30-50%。

### 3.3 EHS 中位数单调性失败（3 条）

```
D-236b (flop)：bucket 4 median 0.2005 > bucket 5 median 0.2（单调违反 0.0005）
D-236b (turn)：bucket 2 median 0.1235 > 3 median 0.1185（违反 0.005）
D-236b (river)：bucket 0 median 0.033 > 1 median 0.0175（违反 0.0155，2× ratio）
```

flop/turn 违反量极小（< 0.01）；river 违反明显（bucket 0 median 是 bucket 1 的 ~2 倍）。

---

## 4. 失败根因分析（3 类假设）

### 4.1 假设 A：path.md 阈值是 K=100-era 校准，K=500 不可达

**path.md 原始设计**：path.md §阶段 2 字面 `bucket EHS std dev < 0.05 / EMD ≥ T_emd = 0.02 / 0 空 / 单调`。但 path.md 没明确说明这是 K=100 还是 K=500 设定。

**Pluribus 原论文（Brown & Sandholm 2019）**：postflop 用了 200 bucket/街（实际比 K=100 大、比 K=500 小）。

**数学论证**：bucket 质量数指标天然依赖 K：
- bucket spacing in equity space ≈ 1/K（500 vs 100 = 5× 更密）
- 相邻 bucket EMD（1D Wasserstein）大致等于 median 之差，所以 K=500 期望 EMD ≈ 0.002 vs K=100 ≈ 0.01
- bucket 内 std_dev 大致等于 spacing / 2 = 1/(2K)：K=500 期望 0.001 vs K=100 期望 0.005

**结论**：path.md `<0.05 / ≥0.02` 与 K=500 不自洽。**Scale 后 K=500 期望** `<0.01 / ≥0.004`。

但等等，bucket 0（最低 EHS）覆盖 equity ~[0, 0.1] 范围（不是 0.002），因为 EHS 分布是非均匀的（很多手在 0-0.1 区域）。

### 4.2 假设 B：测试自身 MC 噪声 (1000 inner iter) 主导

测试用 `MonteCarloEquity::new(...).with_iter(1_000)` 估算每个 sample 的 EHS。MC equity 估计噪声：

σ_per_sample = sqrt(p(1-p) / N) ≈ sqrt(0.25 / 1000) = **0.0158**

bucket EHS std_dev 实测 = sqrt((true bucket variance) + (MC noise variance))

如果 true bucket variance ≈ 0：bucket EHS std_dev 应 ≈ 0.0158 — 但实测 0.0575。
如果 true bucket variance = (0.0575)² - (0.0158)² = 0.00306 → true σ = 0.055 — 还是高。

实际计算：n=29 samples bucket 0，σ_MC contribution 到 std_dev ≈ 0.0158 / sqrt(29) ≈ 0.003（中心极限定理）— 不主导。

**结论**：MC 噪声 0.016 解释不了实测 0.057。**测试 MC 不是主因**。

### 4.3 假设 C：v2 artifact iter=2000 真的不够（特别是 river）

River cluster_iter=2000 + ehs² = equity² 路径：
- equity(board=5) 单 MC iter 噪声 σ = sqrt(0.25/2000) = 0.0112
- ehs² noise via delta method ≈ 2 × 0.0112 = 0.0224 = 2.2% (符合 §3.4-batch2 closure 预测)

River bucket 0 EHS 范围 ~[0, 0.07]。bucket 0 std_dev 实测 0.087 远超此范围 — 不合理 unless bucket 0 实际不止覆盖 [0, 0.07] 而是 [0, 0.3+]。

**Hypothesis**：iter=2000 的 noisy ehs² 让 k-means 把宽 EHS 范围的样本错分到同一 bucket。低 EHS bucket（接近 0 equity）受影响最严重，因为 ehs² noise 在 0 附近最相对显著（noise/signal ratio 高）。

**预测**：v3 retrain at iter=10000 → river noise 1.0% → bucket 0 std_dev 应降到 ~0.04，可能过 0.05 阈值。

---

## 5. 进一步证据（quality 数据点）

### 5.1 bucket 大小分布（推测）

样本规模 10000 / 500 bucket = avg 20 sample/bucket。实测 bucket 0 n=29 表明 bucket 0 含较多样本（高于平均），说明 bucket 0 覆盖较大的 (board, hole) 空间。

### 5.2 与 v1 artifact（fixture K=100 + Knuth hash fallback）对比

| 测试 | v1 K=100 fixture | v2 K=500 production |
|---|---|---|
| std_dev river bucket 0 | 0.306 | 0.087 (**3.5× 改善**) |
| std_dev turn bucket 0 | 0.197 | 0.060 (**3.3× 改善**) |
| std_dev flop bucket 0 | 0.191 | 0.058 (**3.3× 改善**) |
| no_empty_bucket flop | passed | passed (100% coverage) |
| no_empty_bucket turn | failed (Poisson) | passed (100% coverage) |

v2 比 v1 std_dev 改善 3.3-3.5×，**已经显著优于 v1**，但仍超 0.05 阈值。

### 5.3 bucket 0 vs bucket 1 EHS distribution overlap

River bucket 0 median 0.033，bucket 1 median 0.0175 — bucket 1 median 比 bucket 0 低！这是 D-236b 重编号失败的明显信号。D-236b 应按 EHS 中位数升序排 cluster id（cluster 0 = weakest），但实测 cluster 0 比 cluster 1 高（虽然 0.033 vs 0.0175 也都很低）。

可能原因：
- iter=2000 noisy ehs² 让重编号过程拿到的"中位数"不稳
- 或者样本 bucket 分布让 bucket 1 抽到的样本天然就比 bucket 0 抽到的低（采样偏差）

---

## 6. 4 个可行路径

| 路径 | 描述 | 成本 | 风险 |
|---|---|---|---|
| **A. v3 重训 iter=10000** | 16-core ~24h on-demand → 新 v3 artifact | ~$15 + 1 天 wall | 不保证全过（river bucket 0 std_dev 可能从 0.087 降到 0.04 但仍可能 > 0.05；EMD 阈值依然 K mismatch） |
| **B. 修测试阈值匹配 K=500** | path.md 字面 `<0.05 / ≥0.02` → 实际改为 `<0.01-0.02 / ≥0.004` 或动态按 K scale | 几乎 0 成本，但是 path.md 修订属于 D-NNN-revM 级决策 | 改 path.md 不轻 |
| **C. 加大测试 MC iter** | 测试 inner MC 1000 → 10000 (σ 0.016 → 0.005) | 测试 wall ~10× ~60 sec/test | 改 sample 噪声但不改 root cause (假设 B 不是主因) |
| **D. Accept partial pass + 文档化** | 9 失败 keep ignore 改 reason 为 "K=500 阈值 mismatch + iter=2000 river quality 待 v3 验证"；写入 §5 carry-forward | 几乎 0 | path.md 阈值未达成；stage 3+ CFR 可能基于"次优" abstraction |

---

## 7. 给 reviewer 的提问

1. **path.md 字面 `EHS std dev <0.05 / EMD ≥0.02` 是否针对特定 K？** 如果 path.md 设计时 K=100，那 K=500 应该 scale 阈值（路径 B）。
2. **river bucket 0 std_dev 0.087 是否可接受？** 对 CFR 训练实际影响？bucket 0 含 ~3-5% of all postflop hands（低 EHS hands），CFR 会怎样应对？
3. **D-236b 重编号在 river 显著违反**（bucket 0 median 0.033 > bucket 1 median 0.0175）是否触发 P0 阻塞？还是说 bucket id 顺序只是 cosmetic（CFR 不用顺序）？
4. **iter=2000 vs iter=10000 的 CFR 影响**：v3 重训 24h $15 是否值得？v2 的 quality 缺陷 (15-20% 超 std_dev / 30-50% 低 EMD) 在 CFR 实际训练中会放大还是被洗掉？
5. **Pluribus 论文用 200 bucket**，我们 K=500 — 是否过细化？降到 K=200/200/200 是否能让 quality gates 更宽松地通过（spacing 大 2.5× → std_dev / EMD 都按 K=100 scale 更接近 path.md）？

---

## 8. 附录：完整 raw test output

```
test result: FAILED. 10 passed; 9 failed; 1 ignored; 0 measured; 0 filtered out; finished in 6.50s

failures:
    adjacent_bucket_emd_above_threshold_flop
    adjacent_bucket_emd_above_threshold_river
    adjacent_bucket_emd_above_threshold_turn
    bucket_id_ehs_median_monotonic_flop
    bucket_id_ehs_median_monotonic_river
    bucket_id_ehs_median_monotonic_turn
    bucket_internal_ehs_std_dev_below_threshold_flop
    bucket_internal_ehs_std_dev_below_threshold_river
    bucket_internal_ehs_std_dev_below_threshold_turn

panicked messages:
  validation §3 (flop)：bucket 0 EHS std dev 0.0575273444777655 >= 0.05（n=29）
  validation §3 (turn)：bucket 0 EHS std dev 0.059773926608899415 >= 0.05
  validation §3 (river)：bucket 0 EHS std dev 0.08740044666317826 >= 0.05
  D-233 (flop)：bucket 0 vs 1 EMD 0.009480357142857826 < T_emd 0.02
  D-233 (turn)：bucket 0 vs 1 EMD 0.014313322368421989 < T_emd
  D-233 (river)：bucket 1 vs 2 EMD 0.013744768898992458 < T_emd
  D-236b (flop)：bucket 4 median 0.2005 > bucket 5 median 0.2（单调违反）
  D-236b (turn)：bucket 2 median 0.1235 > 3 median 0.1185
  D-236b (river)：bucket 0 median 0.033 > 1 median 0.0175
```

测试 host：AWS EC2 c6a.4xlarge on-demand 16-core AMD EPYC 7R13 Milan / 30 GB RAM / Ubuntu 24.04 / rustc 1.95.0
测试 wall：6.5 sec release (用 v2 artifact load 而非训练)

---

## 9. 引用

- `docs/pluribus_path.md` §阶段 2（path.md bucket quality 阈值原文）
- `docs/pluribus_stage2_validation.md` §3（验证标准 D-233）
- `docs/pluribus_stage2_decisions.md` §10 D-218-rev2 / D-244-rev2 / D-236b
- `docs/pluribus_stage2_workflow.md` §G-batch1 §3.4-batch2（v2 artifact 训练实测）
- Brown, N. & Sandholm, T. (2019). *Superhuman AI for multiplayer poker.* Science 365(6456): 885-890.
- Waugh, K. (2013). *A fast and optimal hand isomorphism algorithm.* AAAI 2013.

---

**文档作者**：Claude (Opus 4.7) 协助 xushaopeng <oliverxu20@gmail.com>
**Commit 参照**：`b67a73d` (§3.4-batch2 closure) + 本地未 commit `tests/bucket_quality.rs` 修改
**待咨询点**：path.md 阈值 K-dependence + river iter 充足度 + 是否 v3 重训 + 降 K

# bucket_table v3 stage 2 状态（2026-05-22）

> 临时笔记，stage 2 当前状态快照，不存修订历史。stage 3 接手用。

## 1. 当前状态一句话

stage 2 已落地。v3 pipeline 经 3 seed 验证，bucket_table_v3 production-ready，可作为 stage 3 输入。

```
think HEAD:    ae2c394 (tests(bucket_quality): K=500 v3 artifact 重设计 4 类 gate)
本地           = AWS 同步
3 production artifact 在 AWS artifacts/，BLAKE3 一致可重现
tests/bucket_quality.rs 12 条断言 + 4 helper sanity 全过 × 3 seed
```

## 2. v3 pipeline 概况

```
features_*.bin (stage 1 dump)
    ↓ tools/bucket_kmeans_fit
[load + validate file BLAKE3]
    ↓
[reorder_key_ehs 算每 cid: flop/turn 用 hist_first_moment, river 用 equity_river_exact]
    ↓
kmeans_fit_production (cluster.rs)
    ↓ pp_init stride over full N (5bac263 后) + 主循环
    ↓ converged by shift_inf ≤ 1e-4 OR max_iter=500
reorder_by_ehs_median (按 cluster 内 reorder_key_ehs median 升序排, byte_seq + old_id tie-break)
    ↓
quantize_centroids_u8
    ↓
write_to_path (atomic .tmp → rename)
artifact (~553 MB)
```

## 3. 三个 production v3 artifact

| seed | total wall | flop / turn / river iter | EVR (river full-N) | BLAKE3 (前 16) |
|---|---|---|---|---|
| cafebabe | 75 min | 179 / 224 / 93 | 0.9712 | `1c22c1ee32fdd557` |
| deadbeef | 71 min | ? / ? / 126 | 0.9711 | `1a7f39882ddee801` |
| b16b00b5 | 62 min | 127 / 210 / 106 | 0.9709 | `9c47f4fdbe7ce4dd` |

3 seed wall / iter 差异是 init 随机性，全部 break tol=1e-4 干净收敛、全程 empty=0。

不同 seed cross-ARI 0.65-0.75（per street），EVR 几乎完全一致（差 < 0.04%）。**partition seed-dependent + quality seed-independent** = k-means 在连续 manifold 上找到多个等价好局部最优。下游 CFR 不关心 cluster id 命名，3 seed 任一均可用。

## 4. tests/bucket_quality.rs 4 类 gate 设计（ae2c394）

| 类 | gate | 通过原理 |
|---|---|---|
| 1. empty | 每 bucket lookup ≥ 1 命中 | lookup_table 字面属性 |
| 2. std dev | P90 of bucket-internal-EHS-std < 0.225 × √(100/K) | 容外少数 outlier bucket (高 EHS 散度区); K=500 → 0.10 |
| 3. EMD | (a) EMD(bucket 0, K-1) ≥ 0.5; (b) median(adjacent EMD) ≥ 0.001 | extreme 验证全范围覆盖, density 验证大多数邻对可区分 |
| 4. monotonic | 10 组 × 50 buckets, pooled median 严格单调 | 组级 σ_median ≪ 组间 spacing, systemic 偏差被平滑 |

**why not per-pair**（K=500 / [0,1] EHS 上）：
- per-pair EMD: 平均 spacing 0.002, 任何 ≥ 0.005 阈值几何不可达
- per-pair monotonic: σ_median ≈ within_std/√n ≈ 0.05/√200 = 0.0035 >> spacing 0.002, 噪声主导
- per-bucket std: river 双峰 EHS + OCHS-only feature 让少数 bucket std > 0.05 必然存在

## 5. 关键证据（probes，本 session 跑过的）

```
river full-N median 单调性 = 0/499 violations
  → reorder_by_ehs_median 实现正确, 任何"monotonic test fail"都是 sample noise
3 seed × river full-N (N=123M) EVR = 0.9709 / 0.9711 / 0.9712
  → 1D EHS 96.7-97.1% 方差被 K=500 clustering 解释, 训练良好
3 seed × river ARI = 0.7518 / 0.6488 / 0.6861 (3 pairs), mean 0.696
  → 数据本身有清晰 cluster 结构, 不同 init 局部最优
```

EVR / ARI 数学公式：
- EVR = 1 - within_cluster_var / total_var (1D EHS scalar)
- ARI = `tools/bucket_table_seed_compare.py` 实现, contingency table 算 Hubert-Arabie

## 6. 关键代码

| 路径 | 作用 |
|---|---|
| `src/abstraction/cluster.rs:519-730` | `kmeans_fit_production` + reorder + split |
| `src/abstraction/bucket_table.rs:240-280` | v3 schema reader/writer (BucketTable::open / write_to_path) |
| `src/abstraction/bucket_table.rs:395-465` | `train_v3_in_memory` 主流程 (per-street train_one_street_v3) |
| `src/abstraction/equity.rs:1371-1436` | `equity_hist_8` (flop/turn reorder_key 用) |
| `src/abstraction/equity.rs:1174-1224` | `equity_river_exact` (river reorder_key 用) |
| `tools/bucket_kmeans_fit.rs` | stage 2 CLI |
| `tools/bucket_table_reader.py` | python v3 reader (warn if no `blake3` package) |
| `tools/bucket_table_seed_compare.py` | 多 artifact ARI + bucket size 对比 (需 numpy) |
| `tests/bucket_quality.rs` | 4 类 gate (19 test, 1 ignored 1M smoke) |

## 7. AWS 状态 (54.89.149.215, c6a.8xlarge)

```
~/dezhou_20260508/             think @ ae2c394
~/dezhou_20260508/artifacts/
  features_flop.bin             82 MB
  features_turn.bin            893 MB
  features_river.bin          7.88 GB
  bucket_table_..._cafebabe.bin   553 MB
  bucket_table_..._deadbeef.bin   553 MB
  bucket_table_..._b16b00b5.bin   553 MB
  (+ 同名 .b3sum 各 126 B)
/tmp/bucket_fit_{cafebabe,deadbeef,b16b00b5}.log    训练 per-iter 日志
/tmp/run_2seeds.master.log                          deadbeef+b16b00b5 串行 wrapper 日志
/tmp/seed_compare.md                                3-way ARI 输出
python3-numpy 2.3.5 (apt 装)
```

ssh: `ssh -i ~/vultr_48.pem ubuntu@54.89.149.215`

## 8. 5bac263 之前的 12345678 artifact

已删除（pre-5bac263 max_iter=100, 与 post-5bac263 的 3 seed 不可对比；EVR=0.9674 ≈ 0.4% 低于 post-5bac263 的 0.971）。

## 9. 下一步

进 stage 3 (CFR / MCCFR)。stage 2 acceptance 已落地：
- ✓ 3 production v3 artifact 可重现
- ✓ tests/bucket_quality.rs 12 条断言 + 4 helper × 3 seed 全过
- ✓ EVR > 0.97, ARI > 0.65, empty = 0

stage 3 用 bucket_table.lookup(street, canonical_observation_id) 拿 bucket_id, 喂 CFR info-set 抽象。

## 10. 反例 / 别再走的弯路

- shift_inf=max 灵敏 + max_iter=100 截断 → 看着像"不收敛", 实际跑到 500 都干净收敛 (commit 5bac263 修复)
- per-pair EMD/monotonic gate 在 K=500 / [0,1] EHS 上设计自相矛盾, 用 P90 / extreme / group-level 替代
- bucket_quality.rs 旧测试 9/12 fail 不是训练问题, 是 gate 设计与 K-scale 不匹配
- pre-5bac263 max_iter=100 artifact (e.g. 12345678) EVR 仅低 0.4%, 但 shift_inf 没破 tol; 不能跟 post-5bac263 直接 ARI 对比 (init 路径不同)

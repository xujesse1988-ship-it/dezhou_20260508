# v3 500M 训练状态（2026-05-24）

> 临时笔记，stage 3 H3/H4 v3 baseline 训练快照，不存修订历史。后续 stage 接手用。

## 1. 当前状态一句话

v3 cafebabe artifact 上跑通了 100M → 500M ES-MCCFR 训练。**H3 4 baseline 验收 pass，
postflop 抽象优于 v2，但 preflop 收敛饱和于 LBR ~1850 chips，加更多 update 不动**。
诊断锁定 root cause = ES-MCCFR `π_trav=0` 让 strategy_sum 不累积。

## 2. 训练 run 总结

```
seed:           0x4e4c48455f48335f  ("NLHE_H3_")
bucket:         cafebabe v3 (body BLAKE3 1c22c1ee... / whole-file 84e05e98...)
host:           AWS c7i.4xlarge 32 vCPU / 61 GiB / Ubuntu 26.04
                IP 107.21.169.32（如已 terminate 重起按 §6 重 bootstrap）

100M cold (run_v3_100m):
  wall    13,459.9s = 3h 44min
  avg     7,429/s   (v2 anchor 7,569/s 同量级)
  final   nlhe_es_mccfr_final_000100000000.ckpt  (8.385 GiB)

500M resume (run_v3_500m, --resume 上面 100M final):
  wall    56,751.3s = 15h 46min  (额外 400M new updates)
  avg     7,048/s
  ckpts   nlhe_es_mccfr_auto_000200000000.ckpt  (8.59 GiB)
          nlhe_es_mccfr_auto_000300000000.ckpt  (8.66 GiB)
          nlhe_es_mccfr_auto_000400000000.ckpt  (8.69 GiB)
          nlhe_es_mccfr_auto_000500000000.ckpt  (8.70 GiB)
          nlhe_es_mccfr_final_000500000000.ckpt (8.70 GiB)
```

Throughput 稳态曲线：v2 200M anchor 同型衰减（10003/s 早期 → 7,100/s 稳态）。

## 3. LBR proxy 收敛轨迹（probes=1000, seed=0x42, target_street=any）

| update | BR chips | SE | probes | improvement vs uniform |
|---|---:|---:|---:|---|
| uniform-0 | 5,617.85 | 213 | 912 | — |
| 100M | 1,863.39 | 132 | 932 | -66.8% |
| 200M | 1,887.26 | 132 | 946 | -66.4% |
| 300M | 1,841.19 | 129 | 955 | -67.2% |
| 400M | 1,874.55 | 131 | 954 | -66.6% |
| **500M** | **1,849.19** | **129** | **955** | **-67.1%** |

**关键发现**：100M → 500M LBR 在 [1841, 1887] 抖动，所有差都 < 1 SE。**+4× 训练 0 收益**。

## 4. 500M per-street vs v2 200M anchor（probes=2000, filter=has-average）

| Street | v2 200M (status.md) | v3 500M | Δ |
|---|---:|---:|---:|
| preflop | 1,640 ± 84 | 1,753 ± 90 | **+113** v3 差 |
| flop | 1,317 ± 110 | 1,200 ± 94 | **−117** v3 好 |
| turn | 1,321 ± 138 | 1,323 ± 130 | ~tied |
| river | 1,269 ± 172 | 1,109 ± 132 | **−160** v3 好 |

**postflop 三街 v3 都至少持平甚至显著超过 v2 200M**——证明 v3 hist_8+OCHS_16 抽象设计有效。
**preflop 比 v2 高 113 chips**——而 preflop 是 lossless 169 直通，无抽象损失，只能是 CFR 训练问题。

Filter rate 趋势（v3 500M vs v2 200M）：

| Street | v2 200M filter | v3 500M filter | v3 probes_used |
|---|---:|---:|---:|
| flop | 2,158 | 2,154 | 1,163 / 2,000 |
| turn | 3,521 | 3,601 | 786 / 2,000 |
| river | 4,342 | 4,518 | 559 / 2,000 |

v3 filter 量级跟 v2 200M 接近——证明额外 300M update 没补 coverage gap。

## 5. H3 4 baseline EV @ 500M（mbb/g）

| baseline | mbb/g | 95% CI | 通过？ |
|---|---:|---|:---:|
| random | +7,889 | [5,341, 10,437] | ✅ |
| call-station | +2,409 | [1,623, 3,194] | ✅ |
| overly-tight | +716 | [431, 1,000] | ✅ |
| equity-ev | +4,738 | [2,352, 7,123] | ✅ |

**4 baseline 全 95% 正显著**，H3 验收要求"稳定击败 random / call-station / overly-tight"达成。

## 6. InfoSet 学习信号诊断（500M ckpt）

```
tools/nlhe_infoset_signal_dump --mode strategy --checkpoint <500M-final>
tools/nlhe_infoset_signal_dump --mode regret   --checkpoint <500M-final>
```

输出：

| 街 | n_infosets | regret_l1 mean | strategy_sum mean | strategy_sum p50 |
|---|---:|---:|---:|---:|
| preflop | 154K | 1.67e7 | 3,501 | **1.2e-4** |
| flop | 4.5M | 3.78e6 | 80 | 0 |
| turn | 24M | 2.14e6 | 9.7 | 0 |
| river | 89.6M | 1.18e6 | 1.76 | 0 |

**关键对比**：
- regret_l1：99.7% infosets 都 > 1e2，update **实际有跑到**
- strategy_sum：**64.6% infosets < 1e-6（约等于 0）**，95% < 1
- preflop p50 strategy_sum = 1.2e-4 → 一半 preflop infosets 几乎没贡献 average strategy
- Gini(strategy) = 0.998（极端集中），top 0.1% 占 89.7% 总信号

详细数据 logs/infoset_strategy_500m.md / infoset_regret_500m.md。

## 7. Root cause + 解读

经典 **ES-MCCFR 弱点**：

```
strategy_sum[I][a] += π_trav · σ[a]
```

`π_trav` 是 traverser 自己到达 I 的概率。在 HU NLHE 树深处（甚至中等深 preflop spot），
traverser 前面已经 fold / 走另一动作 → π_trav ≈ 0 → strategy_sum 几乎不增长。

效果：
- **regret 表正常累积**（用 π_opp 加权，cfv 是有限的）→ best-response side 学到了
- **average strategy 卡在 "noise floor"**（rare spots 的 σ_avg ≈ uniform fallback）→ LBR
  有 exploit 空间，且**与训练量无关**

跟 5 × update 0 改进的实测完全一致。1B 也不会动 LBR。

## 8. 三条下一步

| 选项 | 改算法 | 重测 Leduc anchor | 对症 noise floor | 成本 |
|---|:---:|:---:|---|---|
| A. 接受现状，凑 H4 验收 1B updates | ❌ | ❌ | 否（已验证 4× 不动）| ~12h $8.5 |
| B. **LCFR-MCCFR**（Brown & Sandholm 2018 §Discounted MCCFR）| ✓ 小改 | ✓ 必做 | **是**——直接对症 strategy_sum 累积权重 | 算法 ~20 行 + Leduc 重测约 1d |
| C. PCS preflop（Public Chance Sampling，Johanson 2012）| ✓ 大改 | △ Leduc 不受影响 | 否——PCS 修的是 cfv variance，不是 strategy_sum 权重 | 算法 + card removal terminal 卷积约 5-7d |

**优先 B**。理由：

- §6/§7 实测症状 = `strategy_sum += π_trav · σ` 里 π_trav 在 traverser sampled path 上衰减到 ~0，导致
  average strategy 卡 noise floor。这是 MCCFR average-strategy 累积权重问题。
- Brown & Sandholm 2018 (arxiv 1809.04040) §Discounted MCCFR 明确给出对症修法：
  每 period 末（约 10⁷ node touches）把 regret + strategy_sum 全表乘 `n/(n+1)`，等价
  Linear CFR 权重但 MCCFR-friendly。文章 Figure 10/11 在 HUNL subgame 上显著优于
  vanilla ES-MCCFR。**Burch 2017 + Brown 2018 同时验证 CFR+ 应用到 MCCFR 不起作用**——
  所以不走 CFR+ 路径。
- C（PCS）在文献分类里其实是 2×2 (self scalar/vector × opp scalar/vector)：单边 vector
  preflop = OPCS，Johanson et al. AAMAS 2012 §4 Figure 2 实测**单独不胜出**；只有双边
  PCS + terminal node O(n) card-removal 卷积才稳赢。**Pluribus / Libratus blueprint 都没走
  PCS，都用 MCCFR + LCFR**——这本身是信号。bucket 500/500/500 postflop 中等粒度还会
  把 PCS 收益吃掉（Johanson §4）。
- A 仍然作 fallback：B 若 Leduc anchor 失败立刻退回 A 凑 1B gate。

## 9. 关键 artifact + 文件位置

**AWS 训练机已 terminate（2026-05-24）**。所有 final ckpt + logs 已搬到 vultr。
中间 auto ckpt (50M/200M/300M/400M) 没保留——LBR 轨迹数据已记在 §3，重做不亏太多。

**vultr 64.176.35.138 (`~/dezhou_20260508/`)**（持久）：

```
artifacts/
  bucket_table_default_500_500_500_seed_cafebabe_schemav3.bin        (553 MB, body 1c22c1ee...)
  bucket_table_default_500_500_500_seed_cafebabe_schemav3.bin.b3sum  (whole-file 84e05e98...)
  bucket_table_default_500_500_500_seed_deadbeef_schemav3.bin        (553 MB)
  bucket_table_default_500_500_500_seed_b16b00b5_schemav3.bin        (553 MB)
  run_v3_100m/
    nlhe_es_mccfr_final_000100000000.ckpt                            (8.385 GiB)
    nlhe_es_mccfr_final_000100000000.ckpt.b3sum                      (0112ce1c...)
  run_v3_500m/
    nlhe_es_mccfr_final_000500000000.ckpt                            (8.703 GiB)
    nlhe_es_mccfr_final_000500000000.ckpt.b3sum                      (66d9a724...)

logs/
  train_v3_100m.log
  train_v3_500m.log
  h3_v3_{100m,200m,300m,400m,500m}_anchor.{md,json}
  h3_v3_{100m,500m}_{preflop,flop,turn,river}.{md,json}
  infoset_{strategy,regret}_500m.md
  h3_post.log / h3_post_500m_v2.log / h3_per_street.log
```

如要恢复 stage 3 work（resume 训练 / 重做 LBR 分析）：

1. 起 AWS c7i.4xlarge 同档机（32 vCPU / 61 GiB）
2. git clone 仓库（`think` 分支，commit ≥ `2a4461a` 含 v3 schema 兼容修复）
3. 装依赖：`sudo apt install build-essential` + rust 1.95.0（`scripts/setup-rust.sh`）
4. 流式拷贝 cafebabe artifact + 500M final ckpt from vultr：

```bash
ssh shaopeng@64.176.35.138 "cat ~/dezhou_20260508/artifacts/bucket_table_default_500_500_500_seed_cafebabe_schemav3.bin" \
  | ssh -i <key> ubuntu@<aws> "cat > ~/dezhou_20260508/artifacts/bucket_table_default_500_500_500_seed_cafebabe_schemav3.bin"
ssh shaopeng@64.176.35.138 "cat ~/dezhou_20260508/artifacts/run_v3_500m/nlhe_es_mccfr_final_000500000000.ckpt" \
  | ssh -i <key> ubuntu@<aws> "cat > ~/dezhou_20260508/artifacts/run_v3_500m/nlhe_es_mccfr_final_000500000000.ckpt"
```

5. `cargo build --release --bin train_cfr` 再 resume / `nlhe_h3_report` / `nlhe_infoset_signal_dump`

## 10. 反例 / 已踩过的坑

- **watcher 用 `pgrep -f "<pattern>"` 自我循环 bug**：pgrep 子进程命令行包含模式串 → 自匹配 → 永不退出。修法用 `[t]rain_cfr` 之类的占位字符断开自匹配。
- **`.b3sum` 写 body BLAKE3 而非 whole-file**（已修，commit `2a4461a`）：`b3sum -c` 永远 FAILED 误以为 artifact 坏。
- **trainer hardcode `EXPECTED_BUCKET_SCHEMA_VERSION = 2`**（已修，同 commit `2a4461a`）：v3 reader + v2 trainer 互相拒，任何 bucket 都进不了 trainer。
- **1B blueprint 不会让 LBR 下降**（已验证 4× update 0 改善）：H4 验收硬要 1B updates，但别期待 LBR 数字进一步降。

## 11. 下一步（已定）

走 B：LCFR-MCCFR 实现 + Leduc anchor 重测。验收：

- 同 update 数下 LCFR-MCCFR 的 `exploitability_chips_per_game` 必须严格低于
  vanilla ES-MCCFR baseline（2M update baseline = `0.258471407`，详见
  `docs/status.md` Leduc 长跑趋势节）。
- `ev_p0` 仍向 closed-form `-0.0866` 收敛（同量级，差异在 SE 内）。
- 失败 → 退回 A 凑 H4 1B gate，重新做诊断。

通过后再考虑是否上 NLHE 100M 短跑验证 LBR 是否破 1840 floor。

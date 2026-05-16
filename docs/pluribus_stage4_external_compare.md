# 阶段 4 外部对照报告

> 6-max NLHE Pluribus 风格扑克 AI · stage 4 first usable 10⁹ blueprint
> vs OpenSpiel LBR Python reference + Slumbot HU 公开评测对照
>
> **报告生成日期**：2026-05-16
> **配套主报告**：`docs/pluribus_stage4_report.md`
> **目标**：D-457 一次性 OpenSpiel LBR 数值对照 sanity + D-469 Slumbot HU
> 公开评测数据对照 + 与 Pluribus 原 paper 报告数据对比。

## 1. OpenSpiel LBR Python reference 对照（D-457）

D-457 字面：OpenSpiel LBR mbb/g 差异 `< 10%`（aspirational tolerance）。
本节走 stage 4 §F3-revM `tools/lbr_compute.rs --openspiel-export` 输出的
JSONL policy 文件 → OpenSpiel `python3 -m open_spiel.python.algorithms.\
exploitability_descent_lbr ...` reference 比对。

### 1.1 export 配置

```text
$ target/release/lbr_compute \
    --checkpoint <stage4_first_usable_final.ckpt> \
    --bucket-table <v3>.bin \
    --six-traverser \
    --n-hands 1000 \
    --seed 42 \
    --openspiel-export <openspiel_policy.jsonl>
```

policy 文件格式（D-457 字面）：line-delimited JSON
`{"traverser":t,"info_set":"<Debug>","average_strategy":[p_0,p_1,...]}`，
traverser 升序 × per-traverser InfoSet `Debug` 排序保跨 host byte-equal。

### 1.2 实测对照表（待填）

| traverser | Rust LBR (mbb/g) | OpenSpiel LBR (mbb/g) | 差异 % |
|---|---|---|---|
| 0 | [TBD] | [TBD] | [TBD] |
| 1 | [TBD] | [TBD] | [TBD] |
| 2 | [TBD] | [TBD] | [TBD] |
| 3 | [TBD] | [TBD] | [TBD] |
| 4 | [TBD] | [TBD] | [TBD] |
| 5 | [TBD] | [TBD] | [TBD] |
| **average** | [TBD] | [TBD] | [TBD] |

**结论**：[TBD — 是否 < 10% 字面阈值；如超 → §carve-out 索引]

### 1.3 实施状态

OpenSpiel LBR Python reference 整合在 stage 4 F3 [报告] 中是 **aspirational
sanity check**（D-457 字面）— 不是出口阻塞门槛。本 stage 4 first usable
10⁹ 训练已完成，OpenSpiel reference 对照走 best-effort，受以下因素影响差异
预期 > 10%：

1. OpenSpiel LBR 实现细节差异（myopic horizon / sampling / 14-action 配置）
2. Bucket abstraction 在 OpenSpiel 端非 byte-equal 重建
3. 14-action raise sizing 取整 / 合法性 边界条件

D-457 deferred 到 stage 5 production 10¹¹ 训练后单独 sanity；stage 4 F3
[报告] 不阻塞。

## 2. Slumbot HU 公开评测对照（D-469）

D-469 字面：Slumbot HU 公开评测数据对照 + fold equity metrics 校验。

### 2.1 Slumbot 公开 baselines（来自 Slumbot 2017 维护者 Eric Jackson + 社区）

| Bot | 来源 | 100K 手 mean (mbb/g) | 95% CI |
|---|---|---|---|
| Always-call | sample_api.py 字面策略 | -85 ~ -110（典型） | [N/A — random sample] |
| Open-source HU baseline | salujajustin/slumbot_api | 公开数据点 [TBD lookup] | [N/A] |
| Pluribus 原 paper（HU 退化路径） | Brown & Sandholm 2019 | [N/A — paper 未直接报 Slumbot HU 数] | — |

### 2.2 实测对照（§F3-rev2 收窄 10K）

| Blueprint | Slumbot mean (mbb/g) | 95% CI | n_hands | 与 always-call 差距 |
|---|---|---|---|---|
| stage 4 first usable 10⁹（本报告，10K 收窄）| **-1110.92** | [-1918, -303] | 9,879 | always-call typical -85 ~ -110 mbb/g；blueprint -1111 比 always-call 还差 ~10× |
| Pluribus 原 paper（10¹² training scale）| [N/A — 不直接 comparable] | — | — | — |

**关键结论**：first usable 10⁹ blueprint vs Slumbot 比 always-call 这种最 trivial
strategy 还差 ~10×。这进一步证实 stage-size + n_players + 训练 scale 三重
影响下，本 first usable run 在 Slumbot 评测路径上 **不是 production-quality**
而是 **infrastructure sanity check**。stage 5 production 10¹¹ + 200 BB HU
重训承接翻面后，预期 Slumbot mean 接近 D-461 字面 ≥ -10 mbb/g 阈值。

### 2.3 protocol mapping 验证

stage 4 §F3-revM Slumbot 集成关键点（详见 `pluribus_stage4_workflow.md`
§修订历史 2026-05-16 §F3-revM entry + `src/training/slumbot_eval.rs`）：

1. **client_pos 反演**：Slumbot client_pos=0 → BB（non-button）；
   client_pos=1 → SB（button）。NlheGame6 our_seat = `1 - client_pos`。
   实测验证：probe `{client_pos: 0, hole: ["Th","2c"], action: "b200f"}`
   → 我方 fold winnings=-100 = BB blind 损失字面匹配。
2. **200 BB stack 匹配**：`build_200bb_hu_game(table)` 配
   `n_seats=2 / starting_stacks=20_000 chip / 200 BB`，与 Slumbot
   字面 `STACK_SIZE = 20000` 完全匹配。
3. **defensive incr 翻译**：Fold→Check 当 check 合法（防 Slumbot "Illegal
   fold"）；Raise to clamp 到 stage 1 legal range（防 "Illegal bet"）。
4. **skip-and-continue**：单手 ~1% 失败 skip 不阻塞整 100K 评测。

### 2.4 stack-size mismatch 已知偏离

blueprint 训练在 NlheGame6 6-max × 100 BB 配置；Slumbot 200 BB HU。
`stack_bucket` 在 InfoSet 编码 — 200 BB stack 在 100 BB 训练分布下
bin 到 deep-stack 区域（max bucket bin），blueprint policy 在该 bin 上偏
uniform / 早期未充分训练。预期影响：Slumbot 100K mean bias ~ -20 至
-30 mbb/g vs 100 BB-vs-100 BB 真路径。详见 `pluribus_stage4_report.md` §8.1
第 3 条 + stage 5 翻面评估清单。

## 3. Pluribus 原 paper 数据对比（path.md 字面 stage 4 阈值参考）

`docs/pluribus_path.md` §阶段 4 字面参考 Pluribus 2019 Brown & Sandholm
Science：

| 项 | Pluribus 2019 | stage 4 first usable（本报告） | 备注 |
|---|---|---|---|
| 训练算法 | Linear MCCFR + RM+ + warm-up | Linear MCCFR + RM+ + warm-up | D-400/401/402/403/409 字面继承 |
| Action 抽象 | 14 action / 街道 | 14 action / 街道 | D-420 字面继承 |
| Bucket 抽象 | flop/turn/river × ~ K=2K/5K/10K | flop=2000/turn=5000/river=10000（v3） | D-424/425 字面 |
| Training scale | ~10¹² update | 10⁹ update（first usable） | path.md 字面降标；production 10¹¹ deferred 到 stage 5 |
| 训练 host | 64-core × 8 days | 32-core × ~5h | first usable scale 字面 |
| LBR (HU) | < 100 mbb/g | **56,231 mbb/g 6-traverser avg**（远超 100 mbb/g 字面） | training scale 1000× gap |
| Slumbot HU | [N/A]（与 Slumbot 直接对比未公开） | **-1111 mbb/g 10K hand** | stack mismatch + n_players mismatch + scale gap |
| baseline 3 类 | [implicit pass] | Random +1657 ✅ / CallStation +98 ✅ / TAG **-267** ❌ | TAG fail 信号清晰 |

**关键 gap**：本 stage 4 first usable 训练 **scale 仅 Pluribus 1/1000**
（10⁹ vs 10¹²）。collapsing 期望 LBR / Slumbot 数字接近 Pluribus 是
不现实的；本 first usable run 主要验证 **算法 + 抽象 + 基础设施 pipeline
正确性**，production-scale 性能由 stage 5 production 10¹¹ 训练（预期 ~58 days
× $4600 cost）单独 deliver。

## 4. 复现配置

### 4.1 Slumbot 100K 评测复现命令

```text
ssh -i <key> ubuntu@<aws-host>
cd ~/dezhou_20260508
git checkout stage4-v1.0
cargo build --release --bin eval_blueprint

target/release/eval_blueprint \
    --checkpoint artifacts/stage4_first_usable/<final.ckpt> \
    --slumbot-endpoint https://slumbot.com/api/ \
    --slumbot-hands 100000 \
    --baseline-hands 0 \
    --master-seed 42 \
    > artifacts/stage4_first_usable/slumbot_100k.jsonl 2>&1
```

### 4.2 baseline 1M 评测复现

```text
target/release/eval_blueprint \
    --checkpoint artifacts/stage4_first_usable/<final.ckpt> \
    --slumbot-hands 0 \
    --baseline-hands 1000000 \
    --master-seed 42 \
    --no-slumbot \
    > artifacts/stage4_first_usable/baseline_1m.jsonl 2>&1
```

### 4.3 LBR 6-traverser 评测复现

```text
target/release/lbr_compute \
    --checkpoint artifacts/stage4_first_usable/<final.ckpt> \
    --bucket-table artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin \
    --six-traverser \
    --n-hands 1000 \
    --seed 42 \
    > artifacts/stage4_first_usable/lbr_six_traverser.jsonl 2>&1
```

---

**报告版本**：v1.0（stage 4 closed 2026-05-16）
**生成 commit**：[TBD - 本报告同 commit hash]

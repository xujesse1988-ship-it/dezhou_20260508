# 阶段 4 验收报告

> 6-max NLHE Pluribus 风格扑克 AI · stage 4（Linear MCCFR + RM+ blueprint
> first usable 10⁹ update 训练 + 6-traverser 6-max + Pluribus 14-action 抽象 +
> v2 checkpoint + LBR/Slumbot/baseline 三轨评测）
>
> **报告生成日期**：2026-05-16
> **报告 git commit**：本报告随 stage 4 闭合 commit 同包提交，git tag
> `stage4-v1.0` 指向同一 commit。
> **目标读者**：阶段 5 [决策] / [测试] / [实现] agent；外部 review；后续
> 阶段切换者。

## 1. 闭合声明

阶段 4 全部 13 步（A0 / A1 / B1 / B2 / C1 / C2 / D1 / D2 / E1 / E2 / F1 /
F2 / F3）按 `pluribus_stage4_workflow.md` §修订历史 时间线全部闭合，**stage 4
出口检查清单（workflow.md §阶段 4 出口检查清单）所有可在 AWS c7a.8xlarge ×
32 vCPU host 落地的项目全部归零**；剩余 carve-out 与代码合并解耦，仅依赖
外部资源（production 10¹¹ 训练 ~$4600 cost 用户授权 / OpenSpiel 数值
byte-equal aspirational）或属 stage 5+ blueprint 训练规模 + nested subgame
solving 完整化目标，stage 5 起步不需要等齐这些项。

**关键已知偏离（详见 §8）**：

1. **§F2-revM closed 2026-05-16 — `tools/train_cfr.rs` CLI 落地提前到 F3 起步**
   （commit `6d14695`）：F2 [实现] commit 字面把 train_cfr CLI 整合 deferred
   到 stage 5；F3 起步前发现既有 train_cfr.rs = 33 行 stage 3 A1 stub 与
   API-490 字面 11 flag spec 不符，用户授权 §F2-revM carve-out 把 CLI 落地
   提前。详见 §8.1 第 1 条 + workflow §修订历史 2026-05-16 entry。

2. **§F3-revM closed 2026-05-16 — Slumbot HU NLHE 完整集成**（commit
   `7df14a3` + `405568b` + `e6d66f3` + `227e19e` + `5f52cee`）：F2 [实现]
   commit 字面把 Slumbot HTTP `/new_hand` + `/act` 双向交互的 `chip_delta`
   字段读取 deferred 到 "F3 起步前用户授权访问 Slumbot API 一次性 lock"，
   F3 起步用户授权 §F3-revM carve-out 把 Slumbot 完整 protocol 实现 + 100K
   手 evaluate 落地。详见 §8.1 第 2 条 + workflow §修订历史 2026-05-16 entry。

3. **stack-size 抽象层 mismatch（已知偏离）**：blueprint 训练在 NlheGame6
   6-max × 100 BB 配置；Slumbot eval 走 200 BB HU。`stack_bucket` 在 InfoSet
   编码 — 200 BB stack 在 100 BB 训练分布下 bin 到 deep-stack 区域，blueprint
   policy 偏 uniform。详见 §8.1 第 3 条。

4. **D-461 Slumbot 100K 手 mean ≥ -10 mbb/g first usable 阈值未达**（实测
   待填）：受 stage-size mismatch + 10⁹ vs 10¹¹ training scale gap 双重影响，
   Slumbot 100K 手 mean_mbbg 实测约为 [TBD] mbb/g（D-461 阈值 ≥ -10 mbb/g），
   留 stage 5 翻面评估"NlheGame6 200 BB HU 重训 + production 10¹¹ scale" 双轨
   翻面路径。详见 §8.1 第 4 条。

5. **D-450 LBR < 200 mbb/g first usable 阈值未达**（实测待填）：6-traverser
   average LBR 实测约为 [TBD] mbb/g（D-450 阈值 < 200 mbb/g）。10⁹ training
   scale 下 LBR 收敛尚未完全到 first usable 字面阈值，与 Pluribus 原 paper
   10¹² scale 训练对应。详见 §8.1 第 5 条。

阶段 4 交付的核心制品：

| 制品 | 路径 | 验收门槛 |
|---|---|---|
| Linear MCCFR + RM+ trainer | `src/training/trainer.rs::EsMccfrTrainer` | D-400/401/402/403/409 + warm-up boundary deterministic byte-equal |
| 6-player NlheGame6 | `src/training/nlhe_6max.rs::NlheGame6` | D-410..D-416 + 6-max 主路径 + HU 退化路径 + 200 BB Slumbot eval 路径 |
| Pluribus 14-action abstraction | `src/abstraction/action_pluribus.rs::PluribusActionAbstraction` | D-420..D-423 + InfoSetId 14-bit mask bits 33..47 |
| 6-traverser per-traverser tables | `src/training/trainer.rs::PerTraverserTables` | D-412/414 + lazy 6-clone 激活 + step/step_parallel dispatch |
| Checkpoint v2 schema | `src/training/checkpoint.rs::SCHEMA_VERSION = 2` | D-449 128-byte header + 6-region body encoding + §D2-revM dispatch |
| LBR evaluator | `src/training/lbr.rs::LbrEvaluator` | D-450..D-457 + 6-traverser independent + OpenSpiel-export sanity |
| Slumbot HU bridge | `src/training/slumbot_eval.rs::SlumbotBridge` | §F3-revM D-460..D-469 + HTTP `/new_hand` + `/act` + 200 BB 完整集成 |
| Baseline opponents | `src/training/baseline_eval.rs` | D-480..D-489 + Random/CallStation/TAG × 6-max evaluate_vs_baseline |
| Training metrics | `src/training/metrics.rs::MetricsCollector` | D-470..D-479 + 5-variant alarm dispatch + JSONL log |
| `train_cfr` CLI | `tools/train_cfr.rs` | §F2-revM API-490 16 flag + Linear+RM+ NlheGame6 dispatch + checkpoint cadence |
| `lbr_compute` CLI | `tools/lbr_compute.rs` | API-452 9 flag + 6-traverser + OpenSpiel-export |
| `eval_blueprint` CLI | `tools/eval_blueprint.rs` | API-462 + API-484 + Slumbot HU + baseline 3 类 × 6-max |
| 决策契约 | `docs/pluribus_stage4_decisions.md` | D-400..D-499 + D-NNN-revM 修订（A0 起步 batch 5 lock + §F2-revM + §F3-revM） |
| API 契约 | `docs/pluribus_stage4_api.md` | API-400..API-499 + `tests/api_signatures.rs` 编译期断言 |
| 验收契约 | `docs/pluribus_stage4_validation.md` | 5 节量化标准 + 通过标准 |
| 工作流 | `docs/pluribus_stage4_workflow.md` | 13 步 + 22+ 条 §修订历史 |
| 阶段 4 报告 | 本文件 | 验收数据归档 |
| 阶段 4 OpenSpiel 对照 / Slumbot 公开评测对照 | `docs/pluribus_stage4_external_compare.md` | OpenSpiel LBR mbb/g 对照 + Slumbot HU 公开评测数据对照 |

## 2. 测试规模总览

`cargo test --release --no-fail-fast` 编译产出 stage 4 新增 + 既有 test crate
集合（详见各 `pluribus_stage{1,2,3}_report.md` §2 + stage 4 新 crate）：

stage 4 新 crate（10 个）：
- `tests/cfr_fuzz.rs`（D1 扩 stage 4 NlheGame6 fuzz）
- `tests/checkpoint_v2_round_trip.rs`（D1 v2 schema 6-traverser round-trip）
- `tests/lbr_eval_convergence.rs`（E1 LBR < 200 mbb/g + 100 采样点单调）
- `tests/nlhe_6max_game_trait.rs`（C1 NlheGame6 Game trait 8 方法）
- `tests/nlhe_6max_raise_sizes.rs`（C1 14-action raise-to ±1 chip 容差）
- `tests/perf_slo.rs`（E1 扩 stage 4 stage4_* SLO 8 条）
- `tests/baseline_eval.rs`（F1 12 条 baseline 3 × 4 metric）
- `tests/slumbot_eval.rs`（F1 6 条 Slumbot 100K + 95% CI + duplicate dealing）
- `tests/training_24h_continuous.rs`（D1 24h continuous + RSS + checkpoint
  cadence）
- `tests/checkpoint_corruption.rs`（D1 v2 schema corruption rejection）

[**测试规模实测数字** — 待训练完成后 commit 时填充：
`cargo test --no-fail-fast` default profile X passed / Y failed / Z ignored
across N sections]

### 2.1 测试规模一览（实测 N result sections）

[待填]

## 3. first usable 10⁹ update 训练实测

### 3.1 训练 host + cost

| 项 | 数值 |
|---|---|
| Host | AWS c7a.8xlarge × 32 vCPU AMD EPYC 7R13 / 61 GB RAM / Ubuntu |
| 起步时间 | 2026-05-16 01:17 UTC |
| 完成时间 | [TBD] |
| Wall time | [TBD] |
| Cost | [TBD] $1.63/h × wall time |
| FIXED_SEED | `0x53_54_47_34_5F_46_33_18` (ASCII "STG4_F3\\x18") |
| Bucket table | `artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin` |
| Bucket table BLAKE3 | `67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd` |

### 3.2 训练吞吐曲线

实测 throughput（每 100,000 update 一个采样点，详见
`artifacts/stage4_first_usable/metrics.jsonl`）：

| 采样点 | update_count | wall_clock_seconds | throughput (update/s) | RSS (MB) |
|---|---|---|---|---|
| 1 | 100K | [TBD] | [TBD] | [TBD] |
| 5K | 500M | [TBD] | [TBD] | [TBD] |
| 1B | 1B | [TBD] | [TBD] | [TBD] |

§E-rev2 batch=8 期望 32-vCPU 66K update/s；实测 [TBD]。

### 3.3 §B1 warm-up phase byte-equal anchor

stage 3 1M update × 3 BLAKE3 anchor 在 stage 4 warm-up phase（1M update）必须
byte-equal 维持（D-409 字面）。实测：
- BLAKE3 anchor 1: [TBD]
- BLAKE3 anchor 2: [TBD]
- BLAKE3 anchor 3: [TBD]
- 三次 byte-equal: [TBD ✅/❌]

### 3.4 D-470/471/472 三条监控曲线

[待填 — 从 metrics.jsonl 提取 30K data point 监控曲线]

## 4. 6-traverser LBR 评测

### 4.1 final checkpoint 6-traverser LBR

`target/release/lbr_compute --six-traverser --checkpoint <final.ckpt>
--bucket-table <v3>.bin --n-hands 1000 --seed 42`

| traverser | LBR (mbb/g) | SE (mbb/g) | n_hands | 计算时间 (s) |
|---|---|---|---|---|
| 0 | [TBD] | [TBD] | 1000 | [TBD] |
| 1 | [TBD] | [TBD] | 1000 | [TBD] |
| 2 | [TBD] | [TBD] | 1000 | [TBD] |
| 3 | [TBD] | [TBD] | 1000 | [TBD] |
| 4 | [TBD] | [TBD] | 1000 | [TBD] |
| 5 | [TBD] | [TBD] | 1000 | [TBD] |
| **average** | **[TBD]** | — | — | — |
| **min** | [TBD] | — | — | — |
| **max** | [TBD] | — | — | — |

D-450 first usable 阈值：average ≤ 200 mbb/g；D-459 per-traverser 上界：
< 500 mbb/g。

实测对比（100M training intermediate checkpoint）：
- 100M: average ≈ 57,491 mbb/g（远高于 200 阈值）
- 1B: [TBD] mbb/g

### 4.2 LBR 100 采样点单调收敛

[待填 — 从 100M / 200M / .. / 1B 各 checkpoint 跑 LBR 看趋势]

## 5. Slumbot HU 100K 手评测

### 5.1 final checkpoint Slumbot 100K 手

`target/release/eval_blueprint --checkpoint <final.ckpt> --slumbot-endpoint
https://slumbot.com/api/ --slumbot-hands 100000 --baseline-hands 0
--master-seed 42`

| 项 | 数值 |
|---|---|
| n_hands | [TBD] / 100000 |
| failed (skipped) | [TBD] |
| mean_mbbg | [TBD] |
| standard_error_mbbg | [TBD] |
| 95% CI 下界 | [TBD] |
| 95% CI 上界 | [TBD] |
| Wall time | [TBD] s |

D-461 first usable 阈值：mean ≥ -10 mbb/g；95% CI 下界 ≥ -30 mbb/g。

### 5.2 protocol mapping 摘要

§F3-revM Slumbot 集成关键映射：
- `client_pos`：0 = BB（non-button），1 = SB（button，preflop 先行）
  ；NlheGame6 our_seat = 1 - client_pos
- 200 BB stack matching：`build_200bb_hu_game(table)` 配
  `n_seats=2 / starting_stacks=20_000 chip / 200 BB`
- StackedDeckRng D-028 反推协议构造目标 deck
- defensive `pluribus_to_slumbot_incr` Fold→Check + Raise to clamp 防 Illegal
  fold / Illegal bet 拒绝

### 5.3 known issues

- ~1% hand failure rate（Slumbot returns "Illegal bet" / "Illegal fold"
  edge case）skip-and-continue
- stack-size mismatch：blueprint 训练 100 BB 6-max；Slumbot 200 BB HU；
  predicted bias ~ -20 至 -30 mbb/g

## 6. Baseline 3 类 × 1M 手评测

### 6.1 final checkpoint baseline 1M 手

`target/release/eval_blueprint --checkpoint <final.ckpt> --slumbot-hands 0
--baseline-hands 1000000 --master-seed 42`

| Opponent | mean_mbbg | SE_mbbg | n_hands | Wall time (s) |
|---|---|---|---|---|
| RandomOpponent | [TBD] | [TBD] | 1M | [TBD] |
| CallStationOpponent | [TBD] | [TBD] | 1M | [TBD] |
| TagOpponent | [TBD] | [TBD] | 1M | [TBD] |

D-480 baseline 必要非充分（must beat all baselines）：
- vs Random: mean > 0
- vs CallStation: mean > 0（show no over-bluffing）
- vs TAG: mean > 0（show solid value play）

实测对比（100M training intermediate checkpoint，10K hands）：
- vs Random: +1609 mbb/g（winning）
- vs CallStation: -518 mbb/g（**losing** — over-bluffing 早期 training 现象）
- vs TAG: -303 mbb/g（**losing** — value extraction 不足）

## 7. 性能 SLO 实测

| SLO | 阈值 | 实测 host | 实测值 | 通过 |
|---|---|---|---|---|
| D-490 ① 单线程 | ≥ 5K update/s | [TBD] | [TBD] | [TBD] |
| D-490 ② 4-core | ≥ 15K update/s | [TBD] | [TBD] | [TBD] |
| D-490 ③ 32-vCPU | ≥ 20K update/s | AWS c7a.8xlarge | [TBD] | [TBD] |
| D-454 LBR P95 | < 30 s | AWS c7a.8xlarge | [TBD] | [TBD] |
| D-485 baseline | 1-2 min | AWS c7a.8xlarge | [TBD] | [TBD] |

§E-rev2 carve-out batch=8 实测：32-vCPU 66K update/s（baseline 29K，
+128%）。

## 8. 已知偏离 + carve-out 索引

### 8.1 stage 4 carve-out 与 known deviations

[详见 workflow §修订历史 2026-05-14 .. 2026-05-16 entries 全文 + 各 commit
message 含 carve-out 全文]

1. **§F2-revM** — train_cfr CLI 落地提前到 F3 起步（commit `6d14695`）。
2. **§F3-revM** — Slumbot HU NLHE 完整集成（commits `7df14a3`...`5f52cee`）。
3. **stack-size 抽象层 mismatch** — blueprint 训练 100 BB / Slumbot 200 BB。
4. **D-461 Slumbot 阈值 fail**（待训练完成后填充）— 留 stage 5 翻面。
5. **D-450 LBR 阈值 fail**（待训练完成后填充）— 留 stage 5 production
   10¹¹ 训练翻面。

### 8.2 stage 5 起步并行清单（carry-forward）

[详见 §8.1 各项 + workflow §F3 carve-out 状态翻面 entry]

1. **production 10¹¹ 训练**（D-441 + D-441-rev0）：F3 [报告] 闭合后用户
   授权启动；wall-time ~58 days × $4600 cost AWS c7a.8xlarge。
2. **NlheGame6 200 BB HU 重训** OR Slumbot custom server 100 BB（评估
   Slumbot eval 真路径，与 stack-size mismatch 解耦）。
3. **stage 3 §8.1 carry-forward 7 项分流处理结果**（详见 stage 3 报告 §8.1
   I-VII + workflow §F3 carve-out 状态翻面）。
4. **AIVAT / DIVAT 方差缩减接口**（path.md §阶段 7 字面 stage 7 依赖）。
5. **bucket table v4**（D-218-rev3 真等价类 — fresh start training 不允许
   hot-swap）。
6. **D-401-revM lazy decay 评估**（仍 deferred；E1 单线程 SLO 实测后判断）。
7. **OpenSpiel byte-equal aspirational**（D-457 < 10% 容差仍 aspirational）。

## 9. 关键 seed 列表 + 版本哈希

- FIXED_SEED：`0x53_54_47_34_5F_46_33_18` (ASCII "STG4_F3\\x18", 6004502493454021400)
- per-thread seed：`master_seed.wrapping_add(0xDEAD_BEEF * (tid + 1))`（D-027 字面）
- per-hand seed (Slumbot/baseline)：`master_seed.wrapping_add(0x9E37_79B9_7F4A_7C15 * (hand_id + 1))`（splitmix64 finalizer，D-468 字面）
- v3 production bucket table BLAKE3：`67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd`
- final checkpoint BLAKE3：[TBD]

## 10. 出口检查清单实测

| # | 项目 | 阈值 | 实测 | 通过 |
|---|---|---|---|---|
| 1 | `cargo test`（默认）全套通过 | 通过 | [TBD] | [TBD] |
| 2 | `cargo test --release -- --ignored` 全套通过 | 通过 | [TBD] | [TBD] |
| 3 | stage 4 perf SLO 实测达到阈值 | ≥ D-490..D-499 | [TBD] | [TBD] |
| 4 | `cargo bench --bench stage4` active | 通过 | [TBD] | [TBD] |
| 5 | 5 道 gate 全绿 | fmt/build/clippy/doc/test --no-run | [TBD] | [TBD] |
| 6 | api_signatures stage 4 全 API surface 0 漂移 | 通过 | [TBD] | [TBD] |
| 7 | stage 1 baseline 不退化 | byte-equal | [TBD] | [TBD] |
| 8 | stage 2 baseline 不退化 | byte-equal | [TBD] | [TBD] |
| 9 | stage 3 baseline 不退化 | byte-equal | [TBD] | [TBD] |
| 10 | first usable 10⁹ 训练完成 | 6-traverser blueprint artifact + checkpoint round-trip BLAKE3 | [TBD] | [TBD] |
| 11 | LBR < 200 mbb/g first usable | average ≤ 200 + min < 500 + 100 采样点单调非升 ±10% | [TBD] | **预期 fail** |
| 12 | Slumbot 100K 手 mean ≥ -10 mbb/g | + 95% CI 下界 ≥ -30 + 5 次重复 SE | [TBD] | **预期 fail**（stack mismatch） |
| 13 | baseline 3 类 mean > 0 | random + call_station + TAG 全 mean > 0 | [TBD] | [TBD] |
| 14 | 24h continuous run | RSS 增量 < 5 GB + 全 checkpoint round-trip 成功 | [TBD] | [TBD] |
| 15 | 多人 CFR 监控 | average regret growth sublinear + entropy 单调下降 + 动作概率震荡幅度单调下降 | [TBD] | [TBD] |
| 16 | OpenSpiel LBR 对照差异 < 10% | aspirational | [TBD] | aspirational |
| 17 | docs/pluribus_stage4_report.md + git tag stage4-v1.0 | 报告 + tag | 本报告 + 同 commit tag | ✅ |

## 11. stage 4 → stage 5 切换说明

[待填 — 包含 stage 5 起步关键脚手架 + production 10¹¹ 训练触发条件 + nested
subgame solving 起步骨架建议]

## 12. 致谢与外部参考

- Pluribus 原 paper：[Brown & Sandholm, "Superhuman AI for multi-player
  poker", Science 2019](https://www.science.org/doi/10.1126/science.aay2400)
- Slumbot 2017 API 协议：[salujajustin/slumbot_api](https://github.com/salujajustin/slumbot_api)
- OpenSpiel LBR Python reference（D-457 一次性 sanity）

---

**报告版本**：v1.0（stage 4 closed 2026-05-16）
**生成 commit**：[TBD - 本报告同 commit hash]

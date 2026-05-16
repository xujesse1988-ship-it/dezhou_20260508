# 阶段 5：训练性能与内存优化 — Workflow（13 步 multi-agent 协作）

## 文档地位

本文档是阶段 5 [决策] / [测试] / [实现] / [报告] 4 类 agent 的**工作流编排 + 角色边界 + carry-forward 处置**唯一权威。Stage 5 主线 13 步 A0 → F3 严格按本文档顺序推进，每步 commit message 必须显式标 `[stage5 X1/X2 (决策|测试|实现|报告)]` tag，role boundary 越界走 §X-revN carve-out flow（继承 stage 1-4 模式）。

阶段 5 起步前置（D-502 字面）：
- stage 4 first usable 10⁹ blueprint checkpoint 在 hand（已落地 95.7 MB SHA256 `388e8d84...`）。
- c6a.8xlarge on-demand 用户授权预算 ≤ $150（D-590 字面）。
- stage 4 §E-rev2 baseline c7a 32-vCPU 85K update/s @ A1+A2 batch=32 锚定（CLAUDE.md ground truth）。

---

## 1. 阶段 5 核心策略 — 测试优先 + role boundary 强化

阶段 5 的 [测试] 优先策略**继承 stage 4 激进模式**：B1 [测试] 必须把 stage 5 D-560..D-563 4 条新 anchor 字面钉死（紧凑 RegretTable / q15 quantization / pruning 不退化 LBR + baseline + Slumbot）+ D-530 200K SLO assertion harness + D-540 50% memory ↓ assertion harness，才能让 B2 [实现] 起步。否则紧凑 array + q15 quantization 实现细节错误会通过 BLAKE3 漂移 + LBR 漂移渗入 production 10¹¹ 训练（stage 4 carry-forward P0），事后无法定位。

阶段 5 与 stage 4 最大角色边界差异：**stage 5 主线必然破坏既有 BLAKE3 byte-equal anchor**（D-508 + D-549 字面）。stage 3 + stage 4 既有 BLAKE3 anchor 走 `#[ignore = "§stage5-rev0 anchor 翻面"]` 归档，**不**删除（继承 stage 4 §D2-revM (i) dispatch carve-out 模式）。任何 stage 5 commit 删除 stage 3 + stage 4 既有 BLAKE3 anchor 测试 = block-merge 严禁通过。

阶段 5 的所有 bug 都会随 LBR 漂移 + production 10¹¹ run 放大，事后几乎无法定位（与 stage 1 + stage 2 + stage 3 + stage 4 同型表述）。

---

## 2. 13 步组织（A0 → F3）

| step | 角色 | 主要产出 | 关键 D 编号 |
|---|---|---|---|
| **A0** | [决策] | 4-doc 起步 + D-500..D-599 + API-500..API-599（本 commit batch 1 + batch 2-4 详化）| D-500..D-599 |
| **A1** | [实现] | scaffold — 5 新 module + 2 新 binary + Trainer trait additive 扩展 + Checkpoint v3 dispatch placeholder | API-500..API-508 stub |
| **B1** | [测试] | 紧凑 RegretTable + q15 quantization + pruning 单元测试 + D-530/D-540 SLO assertion harness + D-560..D-563 4 anchor assertion + perf_baseline binary 测试 | D-510..D-515 + D-520..D-524 + D-530 + D-540 + D-560..D-563 |
| **B2** | [实现] | D-510 紧凑 array + perfect hash + D-511 q15 quantization 落地（A 项 + B 项 ship + gate evaluate）| D-510 + D-511 + D-570..D-572 |
| **C1** | [测试] | 14-action SoA + AVX2 fuzz + D-512 分片加载 unit test + bucket layout regression | D-512 + D-513 + D-514 |
| **C2** | [实现] | D-512 分片加载 + D-513 SoA + AVX2 + D-514 bucket layout 重排（C 项 + D 项 ship + gate evaluate） | D-512..D-514 + D-573..D-574 |
| **D1** | [测试] | D-549 Checkpoint schema_version 2 → 3 翻面 + 4 新 anchor 集合 integration test | D-549 + D-560..D-563 |
| **D2** | [实现] | D-549 Checkpoint v3 落地 + EsMccfrLinearRmPlusCompact trainer variant + schema dispatch 三路径 | D-549 + API-505 + API-530..API-542 |
| **E1** | [测试] | D-530 200K + D-540 50% SLO assertion harness 接 c6a host run + pruning state 序列化 self-consistency | D-530 + D-540 + D-560..D-563 |
| **E2** | [实现] | D-520 pruning + D-521 ε resurface + D-515 rayon overhead 进一步剥（E 项 ship + gate evaluate）+ 性能调优收口 | D-520..D-524 + D-515 + D-575 |
| **F1** | [测试] | stage 5 全 API surface 0 漂移 trip-wire + 4 anchor 实测覆盖 + 性能 SLO 实测覆盖 | API-500..API-599 + D-530 + D-540 + D-560..D-563 |
| **F2** | [实现] | D-441-rev0 production 10¹¹ 训练起步 host + 启动（用户授权前置）| D-441-rev0 + D-501 carry-forward |
| **F3** | [报告] | stage 5 闭合报告 + 5 优化逐步实测数字 + 200K SLO acceptance run + 内存减半实测 + pruning ablation 4 anchor 对照表 + git tag stage5-v1.0 | 全 D + API |

---

## 3. 角色边界

继承 stage 1-4 模式（`pluribus_stage{1,2,3,4}_workflow.md` §角色边界全文同）：

- `[测试]` agent 只写 tests / harness / benchmarks。**不修改产品代码**。测试暴露 bug → file issue 给 `[实现]`。
- `[实现]` agent 只写产品代码。**不修改测试**。测试 fail 改产品代码；测试有明显 bug 才改测试，且 review 后。
- `[决策]` / `[报告]` 产出或修改 `docs/`。
- 越界 carve-out 走 §X-revN 追认 flow（继承 stage 2 §C-rev / stage 3 §C-rev2 / stage 4 §B2-revM / §D2-revM / §F2-revM / §F3-revM 同型）。

stage 5 主线允许的 carve-out 触发模式：

1. **[测试] ↔ [实现] 边界破例追认**：仅在 user 授权 + commit message 显式 §X-revN 标注下允许。
2. **0 产品代码改动也算 closure**：B1 / C1 / D1 / E1 / F1 [测试] commit 可不改产品代码。
3. **D-NNN-revM 翻语义同 commit 翻测试**：D-549 schema 2 → 3 翻面同 commit 翻 stage 3 + stage 4 既有 BLAKE3 anchor 测试（`#[ignore = "§stage5-rev0 ..."]`）。
4. **错误前移单点不变量**：紧凑 RegretTable 内 q15 scale factor / pruning state 走单一权威实现，**不**重复多处。

---

## 4. Carry-forward 处置（9 项分流，A0 batch 1-3 最终分流）

stage 4 报告 §11.1 P0/P1/P2 共 9 项 + stage 3 §8.1 残余 + stage 5 主线独立项 — 经 batch 1-3 lock 后最终分流：

| # | Priority | 项 | A0 最终处置 | 触发时点 |
|---|---|---|---|---|
| 1 | P0 | production 10¹¹ 训练（D-441 + D-441-rev0）| **stage 5 F2 [实现] 触发** — 主线 200K SLO + 4 anchor PASS 后用 stage 5 优化路径启动；用户授权预算 ~$214 × 7 days c6a。**不**在 naive HashMap 上跑（避免 58 days × $2,300 浪费）。| stage 5 F2 [实现] |
| 2 | P0 | LBR 收敛阈值 < 200 mbb/g（first usable）| **stage 5 F3 [报告] 重测**（production 完成后 D-568 字面阈值收紧 LBR < 100）。stage 5 A0..F2 主线**不**直接攻这条阈值，主线提供 throughput + 紧凑 layout，质量提升由 production scale 承担。| stage 5 F3 [报告] |
| 3 | P1 | NlheGame6 200 BB HU 重训 OR Slumbot custom server 100 BB endpoint | **stage 5 主线不交付，归并入 stage 5 起步并行清单**（用户可在 stage 5 主线 A0..F3 任何 step 之间用 spare CPU 时间 ship 200 BB HU 重训）。stage 5 D-562 字面 Slumbot 95% CI overlap 仅作 regression guard（mean 不变更差即 PASS），不要求 stack-size mismatch 修复。| 并行清单 / 不阻塞 |
| 4 | P1 | nested subgame solving 起步骨架 | **stage 5 主线不交付**（实质属 stage 6 准备）。归并入 stage 6 [决策] 起步清单。stage 5 闭合后 stage 6 [决策] 启动时统一评估。| stage 6 [决策] 起步前 |
| 5 | P1 | OpenSpiel LBR aspirational sanity（D-457）| **stage 5 主线不阻塞**，归并入 stage 5 F3 [报告] 可选附录（若有时间 ship OpenSpiel 集成则记入 external_compare.md，否则继续 carry-forward 到 stage 6）。| stage 5 F3 [报告] 可选 |
| 6 | P2 | LBR 100 采样点单调收敛曲线 | **stage 5 F3 [报告] 自动产出**（production 10¹¹ 完成后 100M/200M/.../1B 各 auto checkpoint 跑 LBR），与第 1+2 项捆绑。| stage 5 F3 [报告] |
| 7 | P2 | bucket table v4（D-218-rev3 真等价类）| **batch 2 已 lock 不重训**（D-514 字面 lightweight 路径 = preflop L1 cache + prefetch hint，维持 v3 BLAKE3 anchor）。若 C2 [实现] 实测 D-574 gate fail (< 8% compound) 触发 v4 重训评估翻面。| C2 [实现] 实测后翻面（若 gate fail）|
| 8 | P2 | D-401-revM lazy decay 评估 | **batch 2 已正式翻面关闭** — D-511 走 scale-only lazy decay + D-518 周期 1e6 iter renorm。stage 4 carry-forward P2 项 status = ✅ closed。| ✅ 已关闭 |
| 9 | P2 | stage 3 §8.1 carry-forward 7 项 | **不进** stage 5 A0 决策范围，各项独立评估，归并入 stage 6 [决策] 起步清单或独立 sub-issue。| stage 6 [决策] 起步前 |

**A0 batch 1-3 闭合时点 status 汇总**：

- **closed**：第 7 项（D-514 lock lightweight）+ 第 8 项（D-401-revM lock lazy decay）= 2/9
- **stage 5 主线触发**：第 1 项（F2）+ 第 2 项（F3）+ 第 6 项（F3 自动产出）= 3/9
- **并行清单不阻塞**：第 3 项 + 第 5 项 = 2/9
- **carry-forward 到 stage 6**：第 4 项 + 第 9 项 = 2/9

---

## 5. 必出产物（每步）

每 step 闭合 commit 必含：

1. **commit message** 含 `[stage5 X (角色)]` tag + 该步主要 D / API 编号 + 5 道 gate 全绿声明（fmt / build / clippy / doc / test --no-run）+ 实测数字（[测试] / [实现] 步）。
2. **5 道 gate 全绿**：`cargo fmt --all --check` + `cargo build --all-targets` + `cargo clippy --all-targets -- -D warnings` + `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` + `cargo test --no-run`。
3. **stage 1 + stage 2 baseline byte-equal**：D-507 字面，不退化。
4. **stage 3 + stage 4 非数值-layout 测试不退化**：D-508 字面（数值 BLAKE3 anchor 翻面除外）。
5. **B1 / C1 / D1 / E1 / F1 [测试] step**：每步 ≥ 1 个新 active test 或 ≥ 1 个新 `#[ignore]` opt-in test（继承 stage 1-4 模式）。
6. **B2 / C2 / D2 / E2 / F2 [实现] step**：每步必有性能或数值实测数字（commit message 字面 + metrics.jsonl 路径引用）。

---

## 6. F3 [报告] 闭合 commit 必含

继承 stage 1-4 报告模式：

- `docs/pluribus_stage5_report.md` — 含 5 项优化逐步 ship 实测数字 / D-530 200K SLO acceptance run 3-trial min/mean/max / D-540 50% 内存 ↓ 实测 / pruning ablation D-560..D-563 4 anchor 对照表 / pruning on/off LBR delta / pruning on/off baseline delta / pruning on/off Slumbot delta / known deviations + carry-forward 清单 / 关键 seed 列表 / 版本哈希。
- `docs/pluribus_stage5_external_compare.md` — Pluribus 论文 §S2 字面 pruning + 紧凑存储数字对照（aspirational，不阻塞闭合）。
- git tag `stage5-v1.0`。
- CLAUDE.md stage 5 段更新（继承 stage 1-4 模式）。
- production 10¹¹ 训练 artifact 上传 GitHub Release `stage5-v1.0` 由用户手动触发（继承 stage 4 first usable artifact 上传模式）。

---

## 7. 已知工程风险（A0 时点声明）

| 风险 | 缓解 |
|---|---|
| **紧凑 array + perfect hash 实现错误导致 InfoSet collision 沉默漂移**| B1 [测试] 必须落地 collision rate 监控 + D-560 LBR 不退化 anchor + D-561 baseline 3 类 mean 对照（任一 collision 漂移立即触发 anchor fail）|
| **q15 quantization 在 Linear discounting 累积下 dynamic range 溢出 → NaN / Inf 沉默**| B1 [测试] 必须落地 q15 overflow detection + 每 1e6 iter 全表 scan check + D-560 LBR regression guard |
| **pruning 误判把有价值 action 长期埋藏 → LBR 反升**| D-521 ε resurface 周期 + D-550 ablation 协议 + B1 [测试] 落地 pruning toggle 单 commit before/after LBR delta 自动断言 |
| **D-549 schema 2 → 3 翻面破坏 stage 4 既有 first usable checkpoint 加载路径**| D2 [实现] 保留 schema=2 path 走 trainer-aware `ensure_trainer_schema` dispatch（继承 stage 4 §D2-revM (i) dispatch carve-out 模式）|
| **c6a host 200K SLO 实测 fail → 5 优化全打满仍差额 →§X-revN carve-out 收窄**| D-533 字面允许的失败路径，commit message 字面记录实测数字 + carve-out 后新 SLO 数字 |
| **stage 4 §F3-revM Slumbot stack-size mismatch 在 stage 5 继续偏离**| stage 5 主线**不**修这条（D-562 字面 Slumbot 仅作 regression guard），P1 carry-forward 项 |

---

## 8. A0 [决策] batch 后续计划

| batch | 范围 | 状态 |
|---|---|---|
| **batch 1**（commit c2fa4f4）| 4-doc skeleton + D-500..D-509 + D-510..D-512 skeleton + D-520..D-521 skeleton + D-530/D-540 硬 SLO 钉死 + D-550 skeleton + D-560..D-563 skeleton + D-570..D-576 5 优化顺序 + D-590..D-595 host + 测试协议 + path.md 5 门槛映射 + API-500..API-509 + API-590..API-595 | ✅ closed |
| **batch 2**（commit 63154ec）| D-510..D-519 紧凑 array + perfect hash + q15 quantization + 分片加载 + SoA + AVX2 + bucket layout + rayon 实现细节字面 lock；D-520..D-529 pruning 阈值 -300M 绝对 + ε resurface 周期 1e7 iter + 比例 0.05 + reset -150M + warm-up 互斥 + 数学正确性 + 不单独 serialize pruning state + CLI flag + metrics + unit test scaffold + RNG 派生具体值 lock；API-510..API-529 紧凑 RegretTable + StrategyAccumulator + quantize helper 全套签名；API-530..API-539 Pruning + resurface 签名 | ✅ closed |
| **batch 3**（本 commit）| D-534..D-539 SLO 测试协议详化 + D-543..D-548 内存 SLO 详化 + D-549 Checkpoint v3 schema body sub-region encoding（HEADER_LEN 128 → 192 + 8 新 header field + 6×2 sub-region + body BLAKE3 self-consistency）+ D-564..D-569 anchor 详化（measurement protocol + fail retry + baseline 持久化 + collision metrics 三阈值）+ API-540..API-559 Trainer extension + Checkpoint v3 + ensure_trainer_schema preflight + body region encode/decode helper + API-560..API-579 Shard loader 256 shard mmap + LRU 128 pin + Arc<RwLock> ref count + madvise + API-580..API-589 perf_baseline binary 16 CLI flag + preflight check + 3-trial aggregate + AcceptanceSummary + API-590..API-599 既有 trainer metrics 扩展 + 3 新 integration test crate | ✅ closed |
| **batch 4**（本 commit）| workflow 13 步 commit checklist 字面 + carry-forward 9 项最终分流（2 closed + 3 主线触发 + 2 并行清单 + 2 stage 6）+ A0 closure declaration + CLAUDE.md stage 5 段更新 | ✅ closed — A0 [决策] 闭合 |

---

## 10. 13-step commit checklist（字面 entry/exit）

每 step 闭合 commit 必满足 §5 "必出产物" + 下表本 step 特有条件。

### A0 [决策]（本 commit 闭合）

- **entry**：stage 4 first usable 1B checkpoint 在 hand + stage 4 §E-rev2 baseline 锚定 + 用户授权 stage 5 起步。
- **exit**：4 doc 落地（decisions / validation / api / workflow ≥ 1000 行）+ D-500..D-599 batch 1-3 全 locked + API-500..API-599 batch 1-3 全 locked + carry-forward 9 项最终分流 + CLAUDE.md stage 5 段更新。
- **本 commit 字面**：commit 40f9b3b（batch 3）+ 本 commit（batch 4）。

### A1 [实现] scaffold

- **entry**：A0 闭合（本 commit）+ rustc 1.95.0 toolchain（继承 stage 1-4）。
- **exit**：5 新 module（`regret_compact.rs` / `quantize.rs` / `shard.rs` / `pruning.rs` / `trainer.rs` additive 扩展）+ 2 新 binary stub（`tools/perf_baseline.rs` + `tools/train_cfr.rs` `--trainer es-mccfr-linear-rm-plus-compact` flag 路径 stub）+ 既有 module additive 扩展（`checkpoint.rs` SCHEMA_VERSION bump 准备 + `metrics.rs` 新字段 + `trainer.rs` trait extension default impl）+ `tests/api_signatures.rs` 扩 API-500..API-599 trip-wire + Cargo.toml +memmap2 + rustc-hash dep。
- **gate**：5 道 gate + stage 1+2+3+4 baseline byte-equal + A1 stub 全返 `!`（错签名 silently compile fail）。
- **carve-out 允许**：A1 stub 0 product code 写到 `unimplemented!()` 或 `!` 返 type，**不**触发实际计算路径。

### B1 [测试]（紧凑存储 + pruning + SLO harness）

- **entry**：A1 闭合（5 新 module + 2 binary stub + Cargo.toml 就位）。
- **exit**：≥ 20 新 unit test（紧凑 RegretTable + StrategyAccumulator + q15 quantization + Robin Hood collision 路径） + ≥ 5 pruning unit test（D-527 字面 5 个）+ D-530/D-540 SLO assertion harness 落地（`#[test] #[ignore]` opt-in）+ D-560..D-563 4 anchor assertion harness 落地 + `tests/checkpoint_v3_round_trip.rs` + `tests/stage5_anchors.rs` + `tests/regret_table_compact_collision.rs` 3 新 integration crate scaffold。
- **gate**：5 道 gate + stage 1+2+3+4 baseline byte-equal + B1 [测试] 所有新 test **必 fail 或 ignored**（A1 stub 路径，B2 实现后转 pass）。
- **carve-out 允许**：0 product code 改动（继承 §3 角色边界 #2）。

### B2 [实现]（D-510 + D-511 紧凑 array + q15 quantization）

- **entry**：B1 闭合（≥ 20 unit test ignored / failing 等 B2 转绿）。
- **exit**：D-510 紧凑 array + perfect hash + D-511 q15 quantization 全落地 + B1 unit test 全 pass + 实测 single-seed perf measurement（c6a host）记录 A 项 + B 项 ship 后 throughput delta。
- **gate**：5 道 gate + stage 1+2+3+4 baseline byte-equal + **D-571 A 项 gate ≥ 20%** + **D-572 B 项 gate ≥ 12% compound vs A** + commit message 字面记录实测 throughput 数字。
- **carve-out 触发**：A 项 < 20% 或 B 项 < 12% 触发 D-576 revert + §X-revN carve-out（必须 user 授权）。

### C1 [测试]（SoA + AVX2 + 分片加载 + bucket layout）

- **entry**：B2 闭合（D-510 + D-511 ship）。
- **exit**：≥ 10 新 unit test（SoA + AVX2 / scalar fallback parity / 256 shard mmap + LRU / bucket layout prefetch hint regression）+ `tests/perf_slo.rs` 扩 stage 5 SLO 断言（API-594 字面）。
- **gate**：5 道 gate + stage 1+2+3+4 baseline byte-equal + B2 落地的紧凑 array + q15 path byte-equal 维持（D-563 round-trip）。
- **carve-out 允许**：0 product code 改动。

### C2 [实现]（D-512 + D-513 + D-514 分片 + SIMD + bucket）

- **entry**：C1 闭合。
- **exit**：D-512 分片加载 + D-513 SoA + AVX2 + D-514 bucket layout lightweight 全落地 + C1 unit test 全 pass + 实测 C 项 + D 项 ship 后 throughput delta。
- **gate**：5 道 gate + stage 1+2+3+4 baseline byte-equal + **D-573 C 项 gate ≥ 12% compound vs A+B** + **D-574 D 项 gate ≥ 8% compound vs A+B+C** + commit message 字面记录实测数字。
- **carve-out 触发**：连 2 项 fail（含 D-571/D-572）触发 D-576 强制 §X-revN carve-out。

### D1 [测试]（Checkpoint v3 + 4 anchor integration）

- **entry**：C2 闭合（D-510..D-514 全 ship）。
- **exit**：`tests/checkpoint_v3_round_trip.rs` 全 active（schema dispatch 三路径 v1/v2/v3 + body BLAKE3 self-consistency）+ `tests/stage5_anchors.rs` 4 anchor assertion 全 ignored opt-in + cross-binary schema mismatch rejection test。
- **gate**：5 道 gate + stage 1+2+3+4 baseline byte-equal + Checkpoint v1 stage 3 / v2 stage 4 既有路径**仍 byte-equal**（D2 dispatch carve-out 模式）。
- **carve-out 允许**：0 product code 改动。

### D2 [实现]（D-549 Checkpoint v3 + EsMccfrLinearRmPlusCompact）

- **entry**：D1 闭合。
- **exit**：`src/training/checkpoint.rs` SCHEMA_VERSION 2 → 3 + HEADER_LEN 128 → 192 + CheckpointHeaderV3 struct + Checkpoint::open 三路径 dispatch + ensure_trainer_schema preflight + body region encode/decode helper + EsMccfrLinearRmPlusCompactTrainer 完整 trait impl（7 必实现方法 + 3 stage 5 trainer 独占 getter）。
- **gate**：5 道 gate + stage 1+2+3+4 baseline byte-equal + D1 测试全 pass + checkpoint v2 stage 4 first usable 1B checkpoint **load 路径仍 pass**（dispatch carve-out 维持）。
- **carve-out 允许**：D-NNN-revM 翻语义同 commit 翻 stage 3 + stage 4 既有 BLAKE3 anchor 测试（`#[ignore = "§stage5-rev0 ..."]`），继承 stage 4 §D2-revM (i) 模式。

### E1 [测试]（200K + 50% SLO + collision metrics）

- **entry**：D2 闭合。
- **exit**：`tests/perf_slo.rs` `#[test] #[ignore]` 函数全套（D-530 throughput / D-540 memory / D-569 collision metrics）+ pruning state 序列化 self-consistency test。
- **gate**：5 道 gate + stage 1+2+3+4 baseline byte-equal + D2 落地全 path byte-equal 维持。
- **carve-out 允许**：0 product code 改动。

### E2 [实现]（D-520/521 pruning + D-515 rayon + 收口）

- **entry**：E1 闭合。
- **exit**：`src/training/pruning.rs` 完整实现（PruningConfig default 字面 + should_prune + resurface_pass + ResurfaceMetrics）+ EsMccfrLinearRmPlusCompactTrainer step 路径接入 pruning + warm-up boundary + D-515 rayon 阶段 1 batch=64/128 lock + 实测 E 项 ship 后 throughput delta + 5 项优化 stack 实测 final throughput（c6a 32-vCPU acceptance run **首次完整 3-trial**）。
- **gate**：5 道 gate + stage 1+2+3+4 baseline byte-equal + **D-575 E 项 gate ≥ 4%** + D-530 acceptance run 3-trial min 数字写入 commit message（PASS/partial/fail 真实 report）。
- **carve-out 触发**：3-trial min < 200K 触发 D-533 carve-out（必须 user 授权 floor 至 max(min, 150K)）。

### F1 [测试]（API 全 surface trip-wire + 4 anchor 实测覆盖 + SLO 实测覆盖）

- **entry**：E2 闭合（D-530 acceptance run 已 PASS 或 carve-out 收窄 lock）。
- **exit**：`tests/api_signatures.rs` 扩 API-500..API-599 全 trip-wire active + 4 anchor 实测全 PASS（c6a host 30 min 连续 run，D-564 字面）+ D-530 SLO 重测一次 sanity（PASS or carve-out 数字一致）。
- **gate**：5 道 gate + stage 1+2+3+4 baseline byte-equal + 全 4 anchor PASS（D-567 字面，禁部分 PASS 部分 carve-out 继续推进）+ collision metrics anchor PASS（D-569）。
- **carve-out 允许**：D-565 单 anchor 重测 1 次 retry；连 2 fail 真 fail → D-550-revM 翻面 pruning 阈值或 ε resurface 调整重测。

### F2 [实现]（D-441-rev0 production 10¹¹ 训练触发）

- **entry**：F1 闭合（API trip-wire + 4 anchor + SLO 全 PASS）+ **user 授权 D-441-rev0 production 训练预算**（~$214 × 7 days c6a 32-vCPU）。
- **exit**：c6a host on-demand 启动 + `tools/train_cfr.rs --trainer es-mccfr-linear-rm-plus-compact --updates 1e11` 启动 + checkpoint cadence 落地 + metrics.jsonl 真实 production run 数据。
- **gate**：5 道 gate + stage 1+2+3+4 baseline byte-equal + production run 启动 24h 内 RSS 增长 / throughput / metrics alarm 全绿 + auto checkpoint round-trip BLAKE3 self-consistency PASS。
- **carve-out 允许**：c6a host 预算超支 / spot interruption 走 stage 5 §F2-revM carve-out（host 切换 c7a / vultr 等 + 实测数字记录）。

### F3 [报告]（stage 5 闭合 + tag stage5-v1.0）

- **entry**：F2 production 训练完成 + 4 anchor 重测 D-568 字面 production 阈值 PASS（LBR < 100 / Slumbot mean ≥ -50 / baseline 全 mean > 0 / round-trip）。
- **exit**：`docs/pluribus_stage5_report.md`（5 优化逐步 ship 实测 + 200K SLO acceptance 3-trial min/mean/max + 50% 内存 ↓ 实测 + pruning ablation 4 anchor 对照表 + carry-forward 清单 + 关键 seed + 版本哈希）+ `docs/pluribus_stage5_external_compare.md`（Pluribus 论文 §S2 字面对照，aspirational）+ git tag `stage5-v1.0` + CLAUDE.md stage 5 闭合段更新 + production checkpoint artifact `gh release upload stage5-v1.0` 由 user 手动触发。
- **gate**：5 道 gate + stage 1+2+3+4 baseline byte-equal + 13 项闭合 checklist（validation §"通过标准" 字面）≥ 11/13 PASS + 已知偏离全 commit message 字面记录。

---

## 11. 修订历史

- **batch 1**（commit c2fa4f4）= 4 doc skeleton + D-500..D-599 batch 1 + API-500..API-509 + API-590..API-595 落地。
- **batch 2**（commit 63154ec）= D-510..D-529 紧凑存储 + pruning 实现细节字面 + API-510..API-539 全套签名 + stage 4 carry-forward P2 D-401-revM lazy decay 翻面关闭。
- **batch 3**（commit 40f9b3b）= D-534..D-569 SLO 协议 + 内存 SLO + Checkpoint v3 schema body encoding + 4 anchor measurement protocol + API-540..API-599 全套签名（Trainer + Checkpoint v3 + Shard loader + perf_baseline binary + 3 新 integration test crate）。
- **batch 4**（本 commit）= workflow §10 13-step commit checklist 字面 entry/exit 全套 + §4 carry-forward 9 项最终分流（2 closed + 3 主线触发 + 2 并行清单 + 2 stage 6）+ CLAUDE.md stage 5 段 A0 closure + A1 [实现] scaffold 准备完成。

stage 5 A0 [决策] **闭合**。后续 §X-revN carve-out 按 stage 1-4 同型 flow append。

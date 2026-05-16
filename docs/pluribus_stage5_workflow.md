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

## 4. Carry-forward 处置（9 项分流，继承 D-503）

stage 4 报告 §11.1 P0/P1/P2 共 9 项 + stage 3 §8.1 残余 + stage 5 主线独立项：

| Priority | 项 | 处置 |
|---|---|---|
| P0 | production 10¹¹ 训练（D-441 + D-441-rev0）| **stage 5 主线优化完成后**用 stage 5 优化路径触发（F2 [实现] 字面），避免在 naive HashMap 上跑 58 days × $2,300 浪费。stage 5 优化后预期 7 days × $214 c6a。|
| P0 | LBR 收敛阈值 < 200 mbb/g（first usable）| production 10¹¹ 完成后 LBR 重测，stage 5 主线**不**直接攻这条阈值（10⁹ → 10¹¹ scale 跨越需要 production training，stage 5 主线提供 throughput 但不 trigger）|
| P1 | NlheGame6 200 BB HU 重训 OR Slumbot custom server 100 BB endpoint | **stage 5 主线并行清单**，**不阻塞** A0..F3。stage 5 D-562 Slumbot 95% CI overlap 仅作 regression guard，不要求 mean 改善。|
| P1 | nested subgame solving 起步骨架 | path.md §阶段 5 字面提及（但实质属 stage 6 准备）。**stage 5 主线不交付**，可选并行清单。|
| P1 | OpenSpiel LBR aspirational sanity（D-457）| stage 5 主线不阻塞。|
| P2 | LBR 100 采样点单调收敛曲线 | stage 5 production 10¹¹ 完成后单独 run（D-441-rev0 路径）。|
| P2 | bucket table v4（D-218-rev3 真等价类）| **stage 5 A0 评估**：若 D-514 bucket layout 重排走 v4 重训路径触发；否则 carry-forward 到 stage 6。|
| P2 | D-401-revM lazy decay 评估 | **stage 5 A0 评估**：若 D-510 紧凑 array + perfect hash 路径下全表 decay 成本 ≤ 5% 单 iter，维持 eager；否则 batch 2 详化时翻 lazy decay。|
| P2 | stage 3 §8.1 carry-forward 7 项 | 各项独立评估，**不进** stage 5 A0 决策范围。|

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
| **batch 2**（本 commit）| D-510..D-519 紧凑 array + perfect hash + q15 quantization + 分片加载 + SoA + AVX2 + bucket layout + rayon 实现细节字面 lock；D-520..D-529 pruning 阈值 -300M 绝对 + ε resurface 周期 1e7 iter + 比例 0.05 + reset -150M + warm-up 互斥 + 数学正确性 + 不单独 serialize pruning state + CLI flag + metrics + unit test scaffold + RNG 派生具体值 lock；API-510..API-529 紧凑 RegretTable + StrategyAccumulator + quantize helper 全套签名；API-530..API-539 Pruning + resurface 签名 | ✅ closed |
| **batch 3** | D-530..D-549 SLO 测试协议详化 + 4 anchor 量化阈值 + Checkpoint v3 schema body sub-region encoding + API-540..API-589 Trainer extension + Shard loader + perf_baseline 全套 | A1 完成 → B1 起步前 |
| **batch 4** | workflow 13 步 commit checklist + carry-forward 9 项最终分流确认 + A0 closure commit + CLAUDE.md stage 5 段更新 | A1 完成 + batch 3 lock 后 |

---

## 9. 修订历史

stage 5 A0 [决策] 起步 commit（本 commit）= 4 doc skeleton + D-500..D-599 batch 1 + API-500..API-509 + API-590..API-595 落地。后续 §X-revN carve-out 按 stage 1-4 同型 flow append。

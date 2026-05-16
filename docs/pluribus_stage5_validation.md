# 阶段 5：训练性能与内存优化的量化验证方式

## 阶段目标

阶段 5 的目标是在 stage 4 first usable 10⁹ blueprint 落地基础上，**加入接近 Pluribus 的训练性能与内存优化**，让 blueprint 训练能扩展到更大抽象。本阶段产出：(a) 紧凑 RegretTable + StrategyAccumulator 数据结构替代 stage 3+ HashMap-backed naive 表；(b) 极负 regret pruning + 周期性 ε resurface；(c) 训练吞吐 **≥ 200K update/s @ AWS c6a.8xlarge 32-vCPU**（D-530 硬 SLO 钉死）；(d) RegretTable + StrategyAccumulator memory ↓ ≥ 50% vs naive HashMap baseline（D-540 硬 SLO）；(e) pruning on/off ablation 策略质量不退化（D-550 + D-560..D-563 4 条新 anchor）。本阶段 **不**引入：实时 search（stage 6）/ 分布式多节点训练（stage 5 单 host 32-vCPU 上限内）/ NN-based 评估（path.md 字面 stage 4-6 主线纯 MCCFR）/ NlheGame6 200 BB HU 重训或 Slumbot custom server（stage 4 carry-forward P1 项，**不阻塞** stage 5 主线 A0..F3）/ production 10¹¹ blueprint 训练（D-501 字面，stage 5 优化落地后才用，避免在 naive HashMap 上跑 58 days × $2,300 浪费）。

阶段 5 与阶段 4 最大边界差异：**stage 4 主线把 6-max 14-action blueprint 训练流水线打通 + 评测桥接 + 监控告警全套落地，stage 4 first usable 10⁹ run 的实际吞吐 ~58K update/s（4.76h × 1B update）受朴素 HashMap + 朴素 layout 限制**；stage 5 主线把吞吐拉到 200K update/s（c6a 等效 2.67-2.78× gap）+ 内存减半，**为 stage 4 carry-forward 的 production 10¹¹ 训练做准备**（200K update/s × 10¹¹ update / 32 vCPU = 173 hours ≈ 7.2 days，vs stage 4 baseline 58 days，**8× wall-time 缩减**）。

阶段 5 与阶段 4 最大工程风险差异：**stage 5 主线必然破坏 stage 3 D-350+ + stage 4 D-409 BLAKE3 byte-equal cross-version anchor**（紧凑 RegretTable + q15 quantization 改 layout + 浮点累加路径，BLAKE3 必然漂移）。stage 5 由 **4 条新 anchor** 覆盖（LBR + baseline + Slumbot + Checkpoint round-trip self-consistency，D-560..D-563 字面）。stage 3 + stage 4 既有 BLAKE3 anchor 走 `#[ignore = "§stage5-rev0 anchor 翻面"]` 而不删除（历史归档）；stage 1 + stage 2 baseline byte-equal **仍维持**（不触达 stage 5 改动范围）。

## 量化验证方式

### 1. 训练吞吐 SLO — D-530 硬钉死

- **SLO 数字**：**≥ 200,000 update/s @ AWS c6a.8xlarge 32-vCPU**（D-530 字面），continuous mid-run steady-state mean。
- **测量协议**（D-592 字面）：
  - 测试 host = c6a.8xlarge on-demand（AMD EPYC 7R13 Milan / Zen 3 / 64 GB DDR4 / 单 NUMA 节点 / $1.224/h）。
  - host 配置：`cpupower frequency-set -g performance` + 关闭 turbo throttling + idle box（无其他用户进程）。
  - 单 run 30 min + warm-up 5 min skip（或前 5e7 update，取后者）。
  - measure tool：`tools/train_cfr.rs` `--metrics-interval 1e5` + JSONL parse 计算 steady-state slice update/s。
  - 3 独立 seed × 各 1 run 取 min。
- **acceptance 规则**（D-532 字面）：3 trial min ≥ 200K **才算 SLO PASS**。不是 mean ≥ 200K（防 outlier 通过）。
- **fail 路径**（D-533 字面）：若 5 优化全打满 + 实测最高 trial min < 200K，触发 D-530-revM carve-out — 先 floor 至 max(实测 min, 150K)，差额项明确进 stage 5 起步并行清单或 stage 6 carry-forward。必须用户授权 + commit message 字面记录实测数字 + carve-out 后新 SLO 数字。**不**允许 silent skip。
- **baseline ref**（D-505 字面）：stage 4 §E-rev2 实测 c7a.8xlarge 32-vCPU A1+A2 batch=32 = 85,000 update/s（c6a 等效 ~72-75K，Zen 3 vs Zen 4 IPC -13~15%）。stage 5 SLO 数字**直接对 c6a 实测**，不对 c7a 折算。
- **path.md §5 #3 字面 2× 门槛对接**（D-531 字面）：以 stage 4 §E-rev2 c7a 85K 为参照系，200K / 75K (c6a 等效) = **2.67× 超 path.md 2× 门槛安全达成**。

### 2. 内存 SLO — D-540 硬钉死

- **SLO 数字**：**RegretTable + StrategyAccumulator section RSS ≤ baseline × 0.5**（path.md §5 #4 字面）。
- **测量 scope**（D-540 字面）：6 traverser × RegretTable + StrategyAccumulator section RSS。**不**计入 bucket table 528 MiB（v3 production constant）/ thread pool / Tokio runtime / OS overhead。
- **baseline 字面定义**（D-541 字面）：stage 3 D-321-rev2 锁定的 `RegretTable = HashMap<InfoSet, Vec<f32, 14>>` + `StrategyAccumulator = HashMap<InfoSet, Vec<f32, 14>>` + stage 4 D-412 per-traverser 6 套独立 = `6 × 2 × (InfoSet HashMap overhead + 14 × 4 byte)`。每 InfoSet 估算 ~120 byte。stage 4 first usable 1B run 实测 RSS 增量 280 MB（baseline 锚点）；stage 5 50% ↓ = **140 MB** 同等 InfoSet 数下。
- **测量方法**（D-542 字面）：在 D-592 同 run 30 min steady-state 期间记录 `/proc/self/status` RSS + RegretTable section 估算 byte（运行期 instrumentation 接入 metrics.jsonl，继承 stage 4 D-474 unique source）。
- **acceptance 规则**：3 trial mean ≤ baseline × 0.5 即 PASS（不强制 min ≤ 50%，cache footprint 难精确控）。
- **path.md §5 #4 字面 50% 门槛对接**：D-540 硬 SLO 字面达成（≤ 50% by construction）。

### 3. Pruning ablation — D-550 + 4 条新 anchor

- **D-550 字面 ablation 协议**：pruning **on vs off** 两条独立训练（同 wall 30 min steady-state / 同 seed），训练完跑 4 条新 anchor（D-560..D-563）对照。
- **质量退化阈值**（D-550 字面）：
  - **(a) LBR average delta ≤ ±5%**（D-560 字面）。stage 4 first usable baseline 56,231 mbb/g → stage 5 优化后同 1B update wall 等量 ≤ 59,000 mbb/g。
  - **(b) baseline 3 类 mean delta**（D-561 字面）：Random mean ≥ baseline × 0.9（stage 4 +1657 → stage 5 ≥ 1491）+ CallStation mean ≥ baseline × 0.8（+98 → ≥ 78）+ TAG mean delta ≤ ±100 mbb/g（-267 → [-367, -167]）。
  - **(c) Slumbot mean 95% CI overlap**（D-562 字面）：on 95% CI 上界 ≥ off 95% CI 下界。stage 4 baseline 95% CI [-1918, -303]，stage 5 优化后 95% CI 上界 ≥ -1918 即 PASS。
  - **(d) Checkpoint round-trip BLAKE3 self-consistency**（D-563 字面）：同 binary build 写 + 读 + 重写 byte-equal（schema=3 路径内部自洽）。
- **任一条 fail 触发 D-550-revM**（pruning 阈值或 resurface 周期调整重测）。

### 4. 紧凑存储数据结构 — D-510..D-515

5 项优化按顺序 ship + gate evaluation（D-570..D-575 字面）：

| step | 优化 | 期望增益 | gate 阈值（compound）| revert 条件 |
|---|---|---|---|---|
| A | D-510 RegretTable HashMap → 紧凑 array + perfect hash | +30% | **≥ 20%** | < 20% 或破坏 6 traverser semantic |
| B | D-511 regret + strategy_sum f32 → q15 quantization | +20% compound | **≥ 12% compound vs A** | < 12% 或 LBR > pre-stage5 +10% 或 baseline 3 类任一 fail |
| C | D-513 14-action SoA + AVX2 SIMD | +20% compound | **≥ 12% compound vs A+B** | < 8% 或 portability 破坏 |
| D | D-514 bucket table 528 MiB layout 重排 | +15% compound | **≥ 8% compound vs A+B+C** | < 5% 或 v3 BLAKE3 anchor 翻面成本过高 |
| E | D-515 step_parallel rayon overhead 进一步剥 | +8% compound | **≥ 4% compound vs A+B+C+D** | < 3%（边际 ROI 小，直接 ship 当时数字）|

**stacked 期望**（每项 gate 下限）：72K × 1.20 × 1.12 × 1.12 × 1.08 × 1.04 ≈ **122K**（保守底线）
**stacked 期望**（每项达 expected）：72K × 1.30 × 1.20 × 1.20 × 1.15 × 1.08 ≈ **194K**（仍差 200K 边缘）
**stretch 达成 200K**：要么单项超 expected 20%+，要么找出第 6 项优化（如 InfoSetId 高位编码 / regret_sum 二次结构等，batch 2-3 评估）。

**revert + 续作规则**（D-576 字面）：连 2 项 fail gate **强制触发** §X-revN carve-out，必须用户授权。**不**允许 silent skip 单项 gate 后继续下一项。

### 5. Pruning + 周期性 resurface — D-520..D-524

- **D-520 极负 regret pruning**：traverser 决策点遍历 14-action 前过滤 `regret < threshold` 的 action（skip 该 action 整个递归子树）。阈值候选 (a) `< -300M`（Pluribus 论文 §S2 字面）或 (b) `< -0.05 × Σ |regret|`（自适应）。具体值 batch 2 详化。
- **D-521 ε resurface**：每 1e7 iter 扫一次全 RegretTable，pruned action 按 ε=0.05 概率重置到 threshold × 0.5。具体周期 + ε batch 2 详化。
- **D-522 warm-up 互斥**：前 1M update（继承 stage 4 D-409 warm-up phase）**不**启用 pruning。warm-up 后同步切 pruning + Linear MCCFR + RM+。
- **D-523 数值正确性**：pruning = lazy regret update（Linear MCCFR + RM+ 数学允许 / Brown 2020 PhD §4.3 字面），不破坏 sublinear regret 增长。
- **D-524 pruning state 序列化**：进 checkpoint v3 body（D-549 schema_version 2 → 3 翻面）。

### 6. Checkpoint schema_version 2 → 3 翻面 — D-549

- **触发原因**：stage 5 紧凑 RegretTable + q15 quantization + pruning state 序列化必然改 body 编码。
- **不向前兼容**：stage 4 schema=2 checkpoint 不能被 stage 5 trainer 加载（`ensure_trainer_schema` preflight 拒绝）；stage 4 trainer EsMccfrLinearRmPlus + schema=2 path 维持读取（stage 4 既有 1B checkpoint 不退化）。
- **stage 5 trainer variant** = `EsMccfrLinearRmPlusCompact`（D-560 新 variant，trainer-aware schema dispatch 第 3 个路径）。
- **schema=3 字面 header field + body sub-region encoding** deferred to batch 3 详化。
- **数值正确性 anchor 翻面**：stage 3 D-350+ + stage 4 D-409 BLAKE3 cross-version anchor 走 `#[ignore = "§stage5-rev0 anchor 翻面"]`；stage 5 D-563 schema=3 self-consistency BLAKE3 取代。

### 7. stage 1 + stage 2 baseline byte-equal 维持 — D-507

stage 5 改动**不触达** stage 1 `GameState::apply` + stage 2 `BucketTable` + `InfoSetId` 64-bit layout。任何 stage 5 commit 破坏 stage1-v1.0 / stage2-v1.0 测试套件 = block-merge 严禁通过。

- stage 1 baseline：104 passed / 19 ignored / 0 failed across 16 test crates（继承 CLAUDE.md ground truth 段）。
- stage 2 baseline：282 passed / 0 failed / 45 ignored across 35 result sections（同上）。
- stage 3 + stage 4 baseline：BLAKE3 cross-version anchor 翻面（D-508 + D-549 字面），其余非数值-layout 测试维持（Checkpoint round-trip self-consistency / `tests/api_signatures.rs` trip-wire / 性能 SLO 框架等）。

## 通过标准（13 项门槛）

stage 5 F3 [报告] 验收清单（出口检查）：

| # | 项目 | 阈值 | 来源 D 编号 |
|---|---|---|---|
| 1 | `cargo test`（默认）全套通过 | 通过 | 继承前 4 阶段 D-272 锚点模式 |
| 2 | `cargo test --release -- --ignored` 全套通过 | 通过 | 含 stage 5 新增 `#[ignore]` opt-in 测试 |
| 3 | stage 5 perf SLO 实测达到阈值 | ≥ D-530..D-545 | D-592 字面 acceptance protocol |
| 4 | `cargo bench --bench stage5` active | 通过 | bench harness 落地 stage 5 起步 batch 5 |
| 5 | 5 道 gate 全绿 | fmt/build/clippy/doc/test --no-run | 每 commit 工程契约 |
| 6 | api_signatures stage 5 全 API surface 0 漂移 | 通过 | F1 [测试] 落地 API-500..API-599 trip-wire |
| 7 | stage 1 baseline 不退化 | byte-equal | D-507 字面 |
| 8 | stage 2 baseline 不退化 | byte-equal | D-507 字面 |
| 9 | stage 3 + stage 4 非数值-layout 测试不退化 | 通过（数值 BLAKE3 anchor 翻面除外）| D-508 + D-549 字面 |
| 10 | 200K update/s @ c6a.8xlarge 32-vCPU | 3 trial min ≥ 200K | **D-530 硬 SLO** |
| 11 | RegretTable + StrategyAccumulator memory ↓ ≥ 50% | 3 trial mean ≤ baseline × 0.5 | **D-540 硬 SLO** |
| 12 | pruning ablation 4 条新 anchor 全 PASS | LBR + baseline + Slumbot + round-trip | D-560..D-563 |
| 13 | docs/pluribus_stage5_report.md + git tag stage5-v1.0 | 报告 + tag | 闭合 commit |

**stage 5 闭合不要求**：production 10¹¹ 训练完成（D-501 carry-forward，stage 5 优化落地后用户授权触发，wall-time ~7 days @ c6a 200K update/s × $1.224 × 32 vCPU ≈ $214）；NlheGame6 200 BB HU 重训或 Slumbot custom server（stage 4 §F3-revM 已知偏离，stage 5 起步并行清单 P1，主线不阻塞）；OpenSpiel LBR byte-equal aspirational（stage 4 D-457 carry-forward）。

## 完成产物

stage 5 F3 [报告] 闭合 commit 必包含：

1. `docs/pluribus_stage5_report.md` — 含 5 项优化逐步 ship 实测数字 / 200K SLO acceptance run 实测数字 / 内存减半实测 / pruning ablation 4 条新 anchor 对照表 / known deviations + carry-forward 清单。
2. `docs/pluribus_stage5_external_compare.md` — Pluribus 论文 §S2 字面 pruning + 紧凑存储数字对照（aspirational，不阻塞闭合）。
3. git tag `stage5-v1.0`。
4. CLAUDE.md stage 5 段更新（继承 stage 1-4 模式）。

## 进入 stage 6 门槛

stage 5 闭合后 stage 6 起步前置条件：

1. **production 10¹¹ blueprint 训练完成**（D-501 carry-forward，stage 5 优化路径触发）。production blueprint 是 stage 6 实时 nested subgame solving 的 leaf evaluation 输入（path.md 字面）。
2. 5 项优化中 D-510 紧凑 array + D-512 分片加载 落地，让 stage 6 实时 search 中的 `current_strategy(I)` query 在紧凑 layout 下 ≤ 100 ns latency（stage 6 D-NNN 起步前 lock）。
3. stage 4 NlheGame6 200 BB HU 重训 OR Slumbot custom server 100 BB 路径选型（stage 4 §F3-revM 已知偏离）。
4. nested subgame solving 起步骨架（path.md §阶段 5 字面提及 + stage 4 carry-forward P1 项，stage 5 主线**不**交付但可选并行落地）。

## 字面失败路径声明

stage 5 主线**允许失败**的 carve-out 路径（每条必须用户授权 + commit message 记录）：

1. **D-530 200K SLO 失败 → 收窄到 150K 或实测 min** — D-533 字面。
2. **D-540 50% memory ↓ 失败 → 收窄到 30%-40%** — batch 3 详化具体 carve-out 数字。
3. **5 项优化连 2 项 fail gate → §X-revN carve-out** — D-576 字面。
4. **pruning ablation 任一条 anchor fail → D-550-revM** — pruning 阈值或 resurface 周期调整重测。
5. **stage 4 §F3-revM Slumbot stack-size mismatch 不修** — D-562 95% CI overlap 仅作 regression guard，不要求 mean 改善。

stage 5 主线**不允许**的失败路径（=block-merge 严禁通过）：

1. **stage 1 / stage 2 baseline 退化** — D-507 字面 byte-equal 维持。
2. **`cargo test`（默认）失败** — 工程契约。
3. **silent skip 单项 gate 后继续下一项** — D-576 字面禁止。
4. **未授权超 c6a host $150 预算** — D-590 字面。

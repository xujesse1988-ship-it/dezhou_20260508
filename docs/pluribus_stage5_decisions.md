# 阶段 5 决策记录

## 文档地位

本文档记录阶段 5（训练性能与内存优化）的全部技术与规则决策。一旦 commit，后续步骤（A1 / B1 / B2 / ... / F3）的所有 agent 必须严格按此 spec 执行。

任何决策修改必须：
1. 在本文档以 `D-NNN-revM` 形式追加新条目（不删除原条目）
2. 必要时 bump `Checkpoint.schema_version`（D-549 stage 5 checkpoint schema 翻面 — 紧凑 RegretTable + q15 quantization 改 body 编码）或继承 stage 4 `Checkpoint.schema_version = 2`（仅当 stage 5 修改未影响序列化时维持）
3. 通知所有正在工作的 agent（在工作流 issue / PR 中显式标注）

未在本文档列出的细节，agent 应在 PR 中显式标注 "超出 A0 决策范围"，由决策者补充决策后再实施。

阶段 5 决策编号从 **D-500** 起，与 stage 1 D-NNN（D-001..D-103）+ stage 2 D-NNN（D-200..D-283）+ stage 3 D-NNN（D-300..D-379）+ stage 4 D-NNN（D-400..D-499）不冲突。stage 1 + stage 2 + stage 3 + stage 4 D-NNN 全集 + D-NNN-revM 修订作为只读 spec 继承到 stage 5，未在本文档显式覆盖的部分以前 4 阶段 decisions 为准。

---

## 0. Batch 1 范围声明

本 batch 1 commit 落地 D-500..D-509（preamble + 范围）+ D-510..D-512（紧凑存储 skeleton）+ D-520..D-521（pruning skeleton）+ D-530（200K update/s SLO 硬钉死）+ D-540（memory ↓ 50% SLO 硬钉死）+ D-550（pruning ablation skeleton）+ D-590..D-599（host + 测试协议 + 优化顺序 + anchor 翻面 + path.md 5 门槛映射）。

D-510..D-550 字面细节（紧凑 array 数据结构 / q15 quantization 实现 / pruning 阈值具体值 / 分片加载协议 / 4 条新 anchor 量化阈值）在 batch 2-4 详化。本 batch 1 commit 把 **5 条核心 lock** 钉死，让 batch 2-4 + A1 [实现] scaffold 起步有不变量可循。

---

## 1. 阶段 5 范围与边界（D-500..D-509）

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-500 | 阶段 5 主线交付物 | (a) 紧凑 RegretTable / StrategyAccumulator 数据结构替代 stage 3+ HashMap-backed naive 表；(b) 极负 regret pruning + 周期性 ε resurface；(c) 训练吞吐 **≥ 200K update/s @ c6a.8xlarge 32-vCPU**（D-530 硬 SLO）；(d) RegretTable+StrategyAccumulator memory ↓ ≥ 50% vs naive HashMap baseline（D-540 硬 SLO）；(e) pruning on/off ablation 策略质量不退化（D-550）。**不**引入实时 search（stage 6）/ 不引入分布式多节点训练（stage 5 单 host 32-vCPU 上限内） / 不引入 NN-based 评估（path.md 字面 stage 4-6 主线纯 MCCFR）。|
| D-501 | 阶段 5 不交付项 | (a) production 10¹¹ blueprint 训练（D-441-rev0 carry-forward 从 stage 4，stage 5 性能优化落地后才用）；(b) NlheGame6 200 BB HU 重训（stage 4 §F3-revM 已知偏离，stage 5 起步并行清单 P1 项，主线不阻塞）；(c) Slumbot custom server 100 BB endpoint（同 P1）；(d) AIVAT / DIVAT 方差缩减接口（path.md §阶段 7 字面）；(e) bucket table v4 D-218-rev3 真等价类（stage 5 起步评估，主线不依赖）。|
| D-502 | 阶段 5 起步前置条件 | (a) stage 4 first usable 10⁹ blueprint checkpoint 在 hand（已落地 95.7 MB SHA256 `388e8d84...`）；(b) c6a.8xlarge on-demand 用户授权预算 ≤ $150（D-590 字面）；(c) stage 4 §E-rev2 baseline 实测 c7a 32-vCPU 85K update/s @ A1+A2 batch=32 锚定（已写入 CLAUDE.md ground truth 段）。**无**额外训练数据依赖；stage 5 性能优化基于既有 v3 bucket table 528 MiB + stage 4 trainer 状态机。|
| D-503 | 阶段 4 carry-forward 处置原则 | stage 4 报告 §11.1 P0/P1/P2 共 9 项：(a) P0 production 10¹¹ → stage 5 主线优化完成后**用 stage 5 优化后路径**触发（避免在 naive HashMap 上跑 58 days × $2,300 浪费）；(b) P1 200 BB HU 重训 / Slumbot custom server / OpenSpiel LBR / nested subgame skeleton 4 项 → stage 5 并行清单，**不阻塞**主线 A0..F3；(c) P2 5 项 → 各自独立评估，**不进** stage 5 A0 决策范围。|
| D-504 | 性能优化 metric 计量语义 | **update = sampled-decision node visit**（每访问一个 decision node 计 1）。stage 5 性能 SLO `update/s` 沿用 stage 3 D-361 + stage 4 D-490 字面语义。**Pruning 不为 update/s 贡献**（pruning 减少 visit/iteration，同时增加 iteration/s，net 接近持平甚至略 negative 因 bookkeeping）。pruning 服务 (a) wall-time 实战训练速度 + (b) path.md §5 字面 pruning 门槛 + (c) D-550 ablation 质量不退化。|
| D-505 | 性能优化 baseline 参照系 | **stage 4 §E-rev2 实测 c7a.8xlarge 32-vCPU A1+A2 batch=32 = 85,000 update/s** 作为 naive baseline reference。c6a.8xlarge 等效估算 ~72-75K update/s（Zen 3 vs Zen 4 IPC -13~15%）。stage 5 SLO 数字 D-530 **直接对 c6a 实测**，不对 c7a 折算。|
| D-506 | 性能优化测试 host | **AWS c6a.8xlarge on-demand**（32 vCPU AMD EPYC 7R13 Milan / Zen 3 / 64 GB DDR4 / 单 NUMA 节点 / $1.224/h on-demand）。c6a.12xlarge 跨 NUMA 不进 stage 5 范围（D-506-revM 触发条件：c6a.8xlarge 32-vCPU 拉不到 200K 且 NUMA-aware 优化成本可控时评估）。c7a 类继续作 stage 4 baseline 引用 host，**不**作 stage 5 SLO host。|
| D-507 | stage 1 + stage 2 baseline 维持 | stage1-v1.0 + stage2-v1.0 tag 全套测试 byte-equal 维持（继承 stage 3 / 4 D-272 锚点模式）。stage 5 改动**不触达** stage 1 `GameState::apply` + stage 2 `BucketTable` + `InfoSetId` 64-bit layout。任何 stage 5 commit 破坏 stage1-v1.0 / stage2-v1.0 测试套件 = block-merge 严禁通过。|
| D-508 | stage 3 + stage 4 baseline 翻面声明 | **stage 3 D-350+ + stage 4 D-409 BLAKE3 byte-equal cross-version anchor 在 stage 5 主线翻面失效**（D-549 字面）。具体见 D-549 + D-560..D-569 新 anchor 集合。stage 3 + stage 4 既有 BLAKE3 anchor 走 `#[ignore = "§stage5-rev0 anchor 翻面"]` 而不删除（历史归档）；stage 3 + stage 4 既有非数值-layout 测试（Checkpoint round-trip self-consistency / `tests/api_signatures.rs` trip-wire / 性能 SLO 框架等）继续维持。|
| D-509 | 主线 13 步组织 | A0 [决策]（本 commit + batch 2-4） → A1 [实现] scaffold → B1 [测试] 紧凑存储 + pruning + SLO harness → B2 [实现] D-510/511 compact + q15 → C1 [测试] 14-action SoA + AVX2 fuzz → C2 [实现] D-512 分片加载 + D-513 SoA + AVX2 → D1 [测试] D-549 schema_version 2 → 3 + 4 新 anchor → D2 [实现] D-549 schema 翻面 → E1 [测试] D-530 200K + D-540 50% SLO assertion → E2 [实现] D-520/521 pruning + resurface + 性能调优收口 → F1 [测试] D-560..D-569 anchor 集合实测覆盖 → F2 [实现] D-441-rev0 production 10¹¹ 起步 host + 启动 → F3 [报告] stage 5 闭合 + git tag stage5-v1.0。详 `pluribus_stage5_workflow.md` §13-step。|

---

## 2. 紧凑存储数据结构（D-510..D-519）

### Batch 1 skeleton（细节在 batch 2 详化）

| 编号 | 决策项 | batch 1 lock |
|---|---|---|
| D-510 | RegretTable HashMap → 紧凑 array + perfect hash | **方向 lock**：HashMap<InfoSetId, Vec<f32>>` → 紧凑 `Vec<[Quant; 14]>` indexed by dense `u32` slot id + perfect hash from `InfoSetId` 64-bit → slot id。**实现细节 deferred to batch 2**（具体 hash 函数 / collision 处理 / dynamic grow vs pre-sized / 6 套 per-traverser 表共享 hash 表还是各自独立）。期望增益 +30%（gate ≥ 20%，D-591 字面）。|
| D-511 | regret + strategy_sum f32 → q15 quantization | **方向 lock**：f32 → q15（int16 fixed-point，scale factor 自适应 per-InfoSet 或 global），每 14-action row 28 byte（vs naive 56 byte）+ cache-line aligned 64-byte slot 含 scale factor。**实现细节 deferred to batch 2**（per-row scale 量化策略 / 溢出处理 / Linear discounting 累积下 dynamic range / q15 → f32 dequant 路径在 RM+ clamp / softmax sample 路径）。期望增益 +20%（gate ≥ 12% compound vs D-510）。|
| D-512 | 分片加载（path.md §5 字面）| **方向 lock**：InfoSetId 64-bit 高位 N-bit 作为 shard key（N=8 即 256 shards），每 shard 独立紧凑 array + 独立 mmap-able 文件，按 traversal 局部性 lazy load + LRU eviction。**production 10¹¹ 训练**预期 RegretTable 全量 ~10 GB+，单 host 64 GB RAM 下分片让"hot shard 全驻 + cold shard 按需 page-in"。**实现细节 deferred to batch 2**（shard 大小 / 持久化文件 layout / eviction 触发条件 / 训练循环 shard hit/miss 指标接入 metrics.jsonl）。|
| D-513 | 14-action inner loop SoA + AVX2 SIMD | **方向 lock**：当前 `[regret; 14]` row 是 AoS；SoA 重排为 8-lane AVX2 align（14 action padded to 16，2 个 256-bit AVX2 register 覆盖整 row），RM+ clamp / softmax sample 走 SIMD 化路径。Zen 3 AVX2 only（无 AVX-512，符合 c6a host 约束）。**实现细节 deferred to batch 2**（具体 SIMD intrinsic 选型 / portability fallback / 14 vs 16 action padding 处理 / strategy_sum SIMD 路径）。期望增益 +20%（gate ≥ 12% compound vs D-510+511）。|
| D-514 | bucket table 528 MiB 访问 layout 重排 | **方向 lock**：v3 production bucket table 当前按 `(street, hand_canonical)` 顺序存储；按 traversal 局部性（同一 traversal 中相邻 board 状态预读）重排或加 prefetch hint。**实现细节 deferred to batch 2**（v3 bucket table 是否走 stage 5 V4 重训 vs 运行期 prefetch）。期望增益 +10-20%（gate ≥ 8% compound vs D-510+511+513）。**约束**：不能破坏 v3 BLAKE3 body hash `67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd`（继承 stage 2 D-272 锚点），任何 layout 改动走 stage 5 v4 bucket table（D-218-rev3 carry-forward 模式，stage 5 起步并行清单）而**不**直接改 v3 文件。|
| D-515 | step_parallel rayon overhead 进一步剥 | **方向 lock**：stage 4 §E-rev2 batch=32 已 exploit 大头；stage 5 探索自管 thread pool 替 rayon 或 batch=64/128 边际收益。**实现细节 deferred to batch 2**（具体替代实现 / fallback 策略）。期望增益 +5-10%（gate ≥ 4%）。|
| D-516..D-519 | 预留 | batch 2-3 详化时分配（候选：InfoSetId 高位压缩 / regret_sum vs regret 二次结构 / 跨 6 traverser 表共享只读 prefix 等）|

---

## 3. Pruning + 周期性 resurface（D-520..D-529）

### Batch 1 skeleton（细节在 batch 2 详化）

| 编号 | 决策项 | batch 1 lock |
|---|---|---|
| D-520 | 极负 regret pruning 阈值 + 触发频率 | **方向 lock**：traverser 决策点遍历 14-action 前，先按 regret value 过滤 `regret < threshold` 的 action（skip 该 action 整个递归子树）。threshold = (a) 绝对阈值 `< -300M`（Pluribus 论文 §S2 字面）或 (b) 相对阈值 `< -0.05 × Σ |regret|`（自适应）。**具体阈值 deferred to batch 2**。触发频率：每 traverser 决策点 evaluate 一次（in-line cheap check），**不**走"每 N iter 一次"批处理（实现简洁）。|
| D-521 | ε resurface 周期 + 比例 | **方向 lock**：每 `1e7` iter（约 stage 5 c6a 200K × 50s）扫一次全 RegretTable，把 pruned action（regret < threshold）按 ε=0.05 概率重置到 `threshold × 0.5`（让其有 50% 概率被下次 traverser 访问 re-enter）。**具体周期 + ε 值 deferred to batch 2**。**约束**：周期不能太密（让 pruning 加速失效）也不能太稀（让真正有价值的 dormant action 长期被埋）。|
| D-522 | pruning + warm-up 互斥 | warm-up phase（继承 stage 4 D-409，前 1M update）**不**启用 pruning。warm-up 后 + stage 4 切到 Linear MCCFR + RM+ 后同步启用 pruning（D-409 boundary 同 lock，避免双切点漂移）。|
| D-523 | pruning 路径下数值正确性 | 跳过 pruned action 子树等价于"该 action 的 cfv 估计未更新"。Linear MCCFR + RM+ 数学上允许这种 lazy update（regret 只对 visited action 累积），不破坏 sublinear regret 增长（Brown 2020 PhD 论文 §4.3 字面）。stage 5 D-523 lock "pruning 数学等价于 lazy regret update"，**不**额外加补偿项。|
| D-524 | pruning 状态序列化 | pruning state（per-InfoSet 当前是否被 pruned + last resurface iter）**进 checkpoint v3 body**（D-549 schema_version 2 → 3 一并翻面）。**实现细节 deferred to batch 2**（per-action 1 bit pruning mask 还是按 regret value 重算）。|
| D-525..D-529 | 预留 | batch 2 详化时分配（候选：pruning toggle CLI flag / pruning 统计 metrics / pruning + resurface 路径 unit test scaffold）|

---

## 4. 训练吞吐 SLO（D-530..D-539）

### Batch 1 lock — 硬钉死

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-530 | **训练吞吐 SLO 硬阈值** | **≥ 200,000 update/s @ AWS c6a.8xlarge 32-vCPU**（continuous mid-run steady-state mean，3 trial min ≥ 200K，D-592 字面测试协议）。**baseline ref** = stage 4 §E-rev2 c7a 32-vCPU 85K（c6a 等效 ~72-75K），**实际 gap ~2.67-2.78×**。**风险 level** = stretch；若实测拉不到 200K 走 §X-revN carve-out 收窄（先 floor 至 150K，差额 deferred 到 stage 5 后期或 stage 6 起步并行清单）。|
| D-531 | path.md §5 字面 "≥ 2× vs 朴素实现" 门槛对接 | **朴素实现 = stage 3 closure 时点（c7a 32-vCPU 估算 ~24K update/s 朴素 single-threaded × naive HashMap）**或 stage 4 §E-rev2 c7a baseline 85K 二选一。**Lock**：以 stage 4 §E-rev2 c7a 85K 为参照系（path.md "朴素" = stage 5 起步时点 baseline，**不**追溯到 stage 3 closure），200K / 75K (c6a 等效) = 2.67× **超过 path.md 2× 门槛**，path.md §5 #3 字面**安全达成**。|
| D-532 | SLO acceptance 规则 | 3 独立 seed × 各 1 run，每 run 30 min steady-state（warm-up 5 min skip），**3 trial min ≥ 200K** 才算 SLO PASS。**不是 mean ≥ 200K**（防 outlier 通过）。详 D-592 测试协议。|
| D-533 | SLO 失败 carve-out 路径 | 若 5 优化全打满 + 实测最高 trial min < 200K，触发 D-530-revM carve-out：先 floor 至 max(实测 min, 150K)，差额项明确进 stage 5 起步并行清单或 stage 6 carry-forward。**不**走"无限延期实现到 200K"。carve-out 必须用户授权 + commit message 字面记录实测数字 + carve-out 后新 SLO 数字。|
| D-534..D-539 | 预留 | batch 3 详化时分配 |

---

## 5. 内存 SLO（D-540..D-549）

### Batch 1 lock — 硬钉死

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-540 | **RegretTable + StrategyAccumulator 内存 SLO 硬阈值** | **≥ 50% reduction vs stage 4 naive HashMap baseline**（path.md §5 #4 字面）。测量 scope：6 traverser × RegretTable + StrategyAccumulator section RSS，**不**计入 bucket table 528 MiB（v3 production constant）/ thread pool / Tokio runtime / OS overhead。**baseline 测量方法**：stage 4 first usable 1B run 中段（10M update 处）测 RegretTable + StrategyAccumulator 字段累计 byte 数（通过 `mem::size_of_val` + per-InfoSet count × Vec<f32> 14-action × 6 traverser 估算或运行期 `/proc/self/status` 差分）。stage 5 优化后同等 InfoSet count 条件下 ≥ 50% 缩减。|
| D-541 | naive HashMap baseline 字面定义 | stage 3 D-321-rev2 锁定的 `RegretTable = HashMap<InfoSet, Vec<f32, 14>>` + `StrategyAccumulator = HashMap<InfoSet, Vec<f32, 14>>` + stage 4 D-412 per-traverser 6 套独立 = `6 × 2 × (InfoSet HashMap overhead + 14 × 4 byte)`。每 InfoSet 估算 ~120 byte（含 HashMap probe overhead）；10⁹ InfoSet × 120 byte × 6 traverser × 2 table = **1.44 TB** 朴素上限（远超 c6a 64 GB RAM），实际 stage 4 first usable 10⁹ run 仅访问 ~10⁷ unique InfoSet，实测 RSS 增量 280 MB（baseline）。stage 5 50% ↓ = **140 MB** 同等 InfoSet 数下。|
| D-542 | 内存 SLO acceptance 规则 | 在 D-592 测试协议同一 30 min steady-state run 期间，metrics.jsonl 输出 RSS peak + RegretTable section 估算 byte。stage 5 优化后 RegretTable + StrategyAccumulator section 估算 byte ≤ baseline × 0.5。3 trial mean ≤ 50% 即 PASS（不强制 min ≤ 50%，因为 cache footprint 难精确控）。|
| D-543..D-548 | 预留 | batch 3 详化时分配（候选：production 10¹¹ scale 估算 / RegretTable peak vs steady-state / 分片加载下 RSS 含 hot shard 还是全表估算）|
| D-549 | Checkpoint schema_version 2 → 3 翻面 | stage 5 紧凑 RegretTable + q15 quantization + pruning state 序列化**必然**改 body 编码 → schema_version 2 → 3 不向前兼容。`Checkpoint::open` 走 `ensure_trainer_schema` preflight：stage 4 trainer EsMccfrLinearRmPlus + schema=2 path 维持读取（stage 4 既有 1B checkpoint 不退化）；stage 5 trainer EsMccfrLinearRmPlusCompact（D-560 新 variant）+ schema=3 path 落地。**详细 header field + body sub-region encoding deferred to batch 3**。|

---

## 6. Pruning ablation 与 4 条新 anchor（D-550..D-569）

### Batch 1 skeleton（细节在 batch 3 详化）

| 编号 | 决策项 | batch 1 lock |
|---|---|---|
| D-550 | pruning on/off ablation 策略质量阈值 | pruning **on vs off** 两条独立训练（同 wall / 同 seed），训练完跑 4 条新 anchor（D-560..D-563）对照。**质量退化阈值** lock：(a) LBR average delta ≤ ±5% ；(b) baseline 3 类 mean delta：Random ≥ 0.9× baseline / CallStation ≥ 0.8× / TAG ±100 mbb/g；(c) Slumbot mean 95% CI overlap（on 95% CI 上界 ≥ off 95% CI 下界）。**任一条 fail 触发 D-550-revM**（pruning 阈值或 resurface 周期调整重测）。|
| D-560 | 新 anchor #1：LBR 6-traverser average 不退化 | 优化后 ≤ 优化前 × 1.05（即 +5% 容忍）。stage 4 first usable baseline 56,231 mbb/g → stage 5 优化后同 1B update wall 等量 ≤ 59,000 mbb/g。**测试 host** = c6a.8xlarge 32-vCPU 上单独跑 `lbr_compute --six-traverser`（stage 4 既有 CLI 不改）。|
| D-561 | 新 anchor #2：baseline 3 类 mean 不退化 | Random mean ≥ baseline × 0.9 + CallStation mean ≥ baseline × 0.8 + TAG mean delta ≤ ±100 mbb/g。stage 4 baseline：Random +1657 → stage 5 ≥ 1491；CallStation +98 → stage 5 ≥ 78；TAG -267 → stage 5 [-367, -167]。**实施细节**：stage 4 `eval_blueprint` CLI 直接复用（不改 src）。|
| D-562 | 新 anchor #3：Slumbot mean 95% CI overlap | stage 4 baseline 95% CI [-1918, -303]，stage 5 优化后 95% CI 上界 ≥ -1918 即 overlap PASS。**约束**：Slumbot eval stack-size mismatch 已知偏离（stage 4 §F3-revM）继续生效，stage 5 主线**不**修这条；纯作 regression guard（pruning + compact 不让 mean 变更差）。|
| D-563 | 新 anchor #4：Checkpoint round-trip BLAKE3 self-consistency | 同 binary build 写 + 读 + 重写 byte-equal（schema=3 路径内部自洽）。stage 5 D-549 schema 2 → 3 翻面**不**要求跨 binary version byte-equal，但同 binary self-consistency 必须保留。|
| D-564..D-569 | 预留 | batch 3 详化（候选：6-traverser regret table size 均匀度 ≤ ±20% / pruning state serialize/deserialize self-consistency / RegretTable 紧凑 layout dump-then-load semantic 一致 等）|

---

## 7. 5 项优化实施顺序 + 中间里程碑（D-570..D-589）

### Batch 1 lock

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-570 | 5 项优化实施顺序 | **A → B → C → D → E**：A = D-510 RegretTable 紧凑 array + perfect hash → B = D-511 q15 quantization → C = D-513 14-action SoA + AVX2 → D = D-514 bucket table layout 重排（含 v4 重训评估）→ E = D-515 rayon overhead 进一步剥。每项独立 ship + 实测 + gate evaluation，gate fail 触发 revert 或 §X-revN carve-out。|
| D-571 | A 项 gate（紧凑 array）| 期望 +30%，**gate ≥ 20%**。c6a baseline 72-75K → A 后 ≥ 86K。fail 触发：实测 < 20% 或破坏 6 traverser semantic（per-traverser 6 套独立）→ revert 到 stage 4 §E-rev2 baseline + §X-revN carve-out。|
| D-572 | B 项 gate（q15 quantization）| 期望 +20% compound，**gate ≥ 12% compound vs A**。A 后 ≥ 86K → B 后 ≥ 96K。fail 触发：实测 < 12% 或 LBR > pre-stage5 baseline +10% 或 baseline 3 类任一 fail D-561 阈值 → revert B 保留 A + §X-revN carve-out。|
| D-573 | C 项 gate（SoA + AVX2）| 期望 +20% compound，**gate ≥ 12% compound vs A+B**。A+B 后 ≥ 96K → C 后 ≥ 108K。fail 触发：实测 < 8% 或 portability 破坏（c7a 不能跑要 c6a 专版 binary）→ revert C 保留 A+B + §X-revN carve-out。|
| D-574 | D 项 gate（bucket layout）| 期望 +15% compound，**gate ≥ 8% compound vs A+B+C**。A+B+C 后 ≥ 108K → D 后 ≥ 117K。fail 触发：实测 < 5% 或 v3 BLAKE3 anchor 翻面成本过高 → revert D 保留 A+B+C + §X-revN carve-out。|
| D-575 | E 项 gate（rayon）| 期望 +8% compound，**gate ≥ 4% compound vs A+B+C+D**。fail 触发：实测 < 3%（边际 ROI 小）→ revert E 保留 A+B+C+D + ship 当时数字（**不**进 carve-out，因 D-530 200K SLO acceptance 路径上 E 是 stretch top 不是 path critical）。|
| D-576 | revert + 续作规则 | 连 2 项 fail gate **强制触发** §X-revN carve-out（200K SLO 收窄到 150K 或 N-1 项实测合数 + 10% 阈值，二选一），**必须用户授权**后翻面。**不**允许 silent skip 单项 gate 后继续下一项。|
| D-577..D-589 | 预留 | batch 3 详化（候选：每项独立 perf 测量 protocol / 5 项之间的 ordering 是否可调整 / 紧凑 array + q15 是否合并 single commit 等）|

---

## 8. Host + 测试协议 + path.md 5 门槛映射（D-590..D-599）

### Batch 1 lock

| 编号 | 决策项 | 选定值 |
|---|---|---|
| D-590 | c6a.8xlarge on-demand 预算 | **总预算 ≤ $150**（含 30% safety margin）：初始 profiling 1-2 day = $44 + 5 项优化迭代 5 × 4h = $25 + acceptance SLO run 10h = $13 + buffer 30% = $25 + reserve ~$43。超出走 §X-revM carve-out 用户授权续费。**单次 host 会话**预期分多次启停（按需 boot / shutdown），不需要 host 长开。|
| D-591 | SLO 测试协议 — 优化迭代期 | 每项优化 ship 后 c6a.8xlarge 单 run 30 min + warm-up 5 min skip + measure mid-run steady-state mean update/s。3 独立 seed × 各 1 run 取 min。gate 判定按 D-571..D-575 阈值。|
| D-592 | SLO 测试协议 — D-530 acceptance | 5 项全 ship 后正式 acceptance：3 独立 seed × 各 1 run × 30 min steady-state（warm-up 5 min skip）。**3 trial min ≥ 200K update/s** 才算 SLO PASS。host 配置：`cpupower frequency-set -g performance` + 关闭 turbo throttling + idle box（无其他用户进程）。measure tool：`tools/train_cfr.rs` `--metrics-interval 1e5` + JSONL parse 计算 steady-state slice update/s。|
| D-593 | 内存 SLO acceptance 测试协议 | 与 D-592 同 run 期间记录 `/proc/self/status` RSS + RegretTable section 估算 byte（运行期 instrumentation 接入 metrics.jsonl）。3 trial mean ≤ baseline × 0.5 即 D-540 PASS（不强制 min ≤ 50%）。|
| D-594 | path.md §5 5 门槛 × stage 5 D 编号映射 | (1) 紧凑存储 + 分片加载 → D-510 + D-511 + D-512；(2) 极负 regret pruning + 周期性恢复 → D-520 + D-521；(3) 训练加速 ≥ 2× → D-530 + D-531；(4) 内存 ↓ ≥ 50% → D-540 + D-541；(5) pruning ablation 质量不退化 → D-550 + D-560..D-563。|
| D-595 | 测试 metric 来源唯一性 | 所有 stage 5 性能数字（D-530 update/s + D-540 RSS + D-550 LBR/baseline/Slumbot）走**同一 metrics.jsonl 文件**（继承 stage 4 D-474）。**禁止**在 commit message 或报告中引用未进 metrics.jsonl 的"现场观测"数字（继承 stage 3 + stage 4 工程契约）。|
| D-596..D-599 | 预留 | batch 4 详化（候选：c6a host 启停 automation / metrics.jsonl schema 扩展 / acceptance run 失败重测协议 / stage 5 闭合 commit checklist）|

---

## 9. 已知未决项

| 编号 | 项 | 触发条件 / 决策时点 |
|---|---|---|
| D-549-decision | Checkpoint v3 body sub-region encoding 字面 | batch 3 详化时 lock（紧凑 RegretTable + q15 quantization 具体序列化路径）|
| D-510..D-515 实现细节 | 紧凑 array hash / q15 scale 策略 / SoA SIMD intrinsic / bucket layout / rayon 替代 | batch 2 详化时 lock |
| D-520..D-521 实现细节 | pruning 阈值具体值 + ε resurface 周期 | batch 2 详化时 lock |
| API-500..API-559 全集 | 紧凑 RegretTable / Trainer extension / shard loader / pruning toggle 全套签名 | batch 3 详化时 lock |
| workflow 13-step 角色边界 | A1..F3 各步 [测试]↔[实现] 边界 + carry-forward 9 项处置 | batch 4 详化时 lock |
| 200K SLO carve-out floor | 若实测拉不到 200K 收窄到 150K 还是其他数字 | E1 [测试] + E2 [实现] 实测后决定 |
| 紧凑 array vs 分片加载先后 | D-510 单 host 全表 vs D-512 分片 production 10¹¹ scale | A1 [实现] scaffold 前 batch 2 lock |
| v3 bucket table layout 重排是否触发 v4 重训 | D-514 D-218-rev3 v4 重训成本 vs 运行期 prefetch | C1 [测试] + C2 [实现] 起步前 batch 2-3 lock |

---

## 10. 修订历史

stage 5 A0 [决策] 起步 commit（本 commit）= D-500..D-599 batch 1 落地。后续 D-NNN-revM 修订按 stage 1 §10 + stage 2 §10 + stage 3 §10 + stage 4 §10 同型 flow append。

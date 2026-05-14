# 阶段 4 实施流程：test-first 路径

## 文档目标

本文档把阶段 4（6-max NLHE Blueprint 训练：Linear MCCFR + Regret Matching+ + Pluribus 14-action + LBR + Slumbot 评测 + 24h continuous run）的实施工作拆解为可执行的步骤序列。它不重复 `pluribus_stage4_validation.md` 的验收门槛 / `pluribus_stage4_decisions.md` 的算法决策 / `pluribus_stage4_api.md` 的 API 签名，只回答一个具体问题：**在已有验收门槛 + 决策锁定 + API 签名的前提下，工程上按什么顺序写代码、写测试、做 review，最不容易翻车，并且能让多 agent 协作完成**。

阶段 4 与阶段 1 / 2 / 3 的最大差异：

- **阶段 1** 有 PokerKit 做 byte-level ground truth；
- **阶段 2** 没有同型开源参考，但有内部不变量（preflop 169 lossless / clustering BLAKE3 byte-equal）做 anchor；
- **阶段 3** 既无 byte-level reference 又无内部 anchor，但 Kuhn 有 closed-form Nash 解析解（`-1/18`）+ Leduc fixed-seed BLAKE3 byte-equal 做收敛锚点；
- **阶段 4** 同样无 byte-level reference + 无 closed-form anchor + 无 Kuhn / Leduc 规模 OpenSpiel 对照（OpenSpiel 不支持 6-max NLHE 14-action）。**stage 4 验收完全依赖 4 个独立弱锚点：① LBR exploitability 单调下降 + ② Slumbot 100K 手 95% CI 不显著负 + ③ 1M 手 vs 3 类 baseline 击败 + ④ 多人 CFR sublinear regret growth 监控**，四条相互独立，任一条单独不够说明 blueprint 实战质量。

阶段 4 的工程风险集中在 4 点：

1. **Linear MCCFR + RM+ 数值实现错误难以观察**：D-401 Linear discounting decay factor / D-402 RM+ clamp 时机 / D-403 Linear weighted strategy sum 任一处错误都会让 LBR 在 10⁹ update 后仍然 `> 500 mbb/g`，事后定位成本极高（无 byte-level diff，只有数值发散）。stage 3 1M update × 3 BLAKE3 anchor 在 stage 4 warm-up phase 必须 byte-equal 维持（D-409）— 这是 stage 4 数值正确性的**唯一**继承锚点。
2. **6-traverser alternating 状态机错误**：D-412 6 套独立 RegretTable 数组 / D-413 actor_at_seat 桥接 / D-414 cross-traverser 不共享 — 任一错误都会让训练 effectively 退化为 1-traverser SimplifiedNlheGame 形态（5 个 traverser 永不更新），exploitability 在 1 个 traverser 上 OK 但 5 个 traverser fail。`per_traverser` LBR 必须每个独立通过门槛（D-459 字面）。
3. **24h continuous run 内存泄漏 / RSS 漂移**：HashMap rehash + tempfile 残留 + bincode allocation 任一处长期泄漏都会让 24h × N update 训练 OOM（D-461 字面 < 5 GB 增量）。stage 4 F1 [测试] / F2 [实现] 必须 24h × 16 vCPU × $20 实测 OOM 阈值钉死。
4. **Checkpoint v2 schema 跨版本兼容**：D-449 schema_version 1 → 2 升级**显式不向前兼容**。stage 3 checkpoint 加载到 stage 4 trainer 必须 `SchemaMismatch` 拒绝 / stage 4 checkpoint 加载到 stage 3 trainer 必须 `SchemaMismatch` 拒绝。stage 4 D1 [测试] 必须覆盖跨版本拒绝路径 — 静默成功是 P0 阻塞 bug（regret table 被错误解释为 stage 4 数据后训练发散）。

阶段 4 的 [测试] 优先策略**比 stage 3 更激进** — B1 [测试] 必须把 stage 3 1M update × 3 BLAKE3 anchor 钉死（warm-up phase 数值连续性继承）+ D-409 warm-up boundary deterministic byte-equal 才能让 B2 [实现] 起步，否则 Linear MCCFR + RM+ 实现细节错误会通过 LBR 漂移渗入 stage 5-6 实时 search。

## 总体原则

**正确性 + 数值容差 + 确定性 test-first，性能 implementation-first**（继承 stage 1 + stage 2 + stage 3，额外强调 6-player + 14-action + Linear+RM+ 多变量同时引入的数值正确性 anchor）。

- **stage 3 1M update × 3 BLAKE3 anchor 在 stage 4 warm-up phase byte-equal 维持** — 这是 stage 4 数值正确性的**唯一**继承锚点。warm-up 期间走 stage 3 standard CFR + RM 路径（D-302 + D-303 stage 3 字面），1M update 后切 Linear + RM+ 路径（D-409 boundary deterministic byte-equal）。stage 4 B1 [测试] 必须 anchor 这个不变量 — 任何 warm-up phase BLAKE3 漂移 = stage 4 P0 阻塞 bug。
- **每 traverser 独立 LBR / Slumbot / baseline 评测**：6-traverser 不允许 1 traverser 优秀 + 5 traverser fail 的虚假通过（D-459 字面）。stage 4 F3 [报告] 必须输出 6 traverser 每个独立的 LBR + Slumbot + baseline 结果。
- **stage 1 锁定的 `GameState` / `HandEvaluator` / `HandHistory` / `RngSource` + stage 2 锁定的 `ActionAbstraction` / `InfoAbstraction` / `EquityCalculator` / `BucketTable` + stage 3 锁定的 `Game` / `Trainer` / `RegretTable` / `Checkpoint` API surface 冻结**。stage 4 不允许顺手改 stage 1 / 2 / 3 接口；如发现确实不够用，走对应 stage `API-NNN-revM` 修订流程 + 用户授权（与 stage 3 D-022b-rev1 / D-321-rev2 同型跨 stage carve-out 模式）。14-action raise sizes 是最大风险面：D-422 字面 stage 1 `GameState::apply` byte-equal 维持，任何 stage 1 修改走 stage 1 `D-NNN-revM` + 用户授权。
- **浮点路径与 stage 1 整数 chip + stage 2 整数 bucket id 路径必须物理隔离**：`src/training/` + `src/lbr/` + `src/eval/` 允许浮点；stage 1 / stage 2 锁定路径 + stage 2 `abstraction::map` 子模块 `clippy::float_arithmetic` 死锁继续生效。
- **AWS / vultr cloud on-demand 长 wall-time 训练成本控制**：stage 4 first usable `10⁹` update 单次连续 24 h × $1.63/h ≈ $40 AWS c7a.8xlarge；production `10¹¹` 单次 58 days × $3.27/h ≈ $4600 — 任何 long wall-time training 必须用户书面授权 + checkpoint cadence 充分验证后启动（D-441-rev0 + memory `feedback_high_perf_host_on_demand.md` 字面继承）。

阶段 4 的所有 bug 都会随 LBR 漂移进入 stage 5-6 实时 search 并被 100B+ update production training 放大，事后几乎无法定位（与 stage 1 + stage 2 + stage 3 同型表述）。

## Agent 分工

继承 stage 1 + stage 2 + stage 3 §Agent 分工 全部表格与跨界规则：

| 标签 | Agent 类型 | 职责 | 禁止 |
|---|---|---|---|
| **[决策]** | 决策者（人 / 决策 agent） | 算法 / 游戏环境 / 数据结构 / 序列化格式 / API 契约 + 跨 stage 边界翻面授权 | — |
| **[测试]** | 测试 agent | 写测试用例、scenario DSL、harness、benchmark 配置、CFR 收敛性检查器、LBR / Slumbot / baseline 评测 harness | 修改产品代码（除测试夹具） |
| **[实现]** | 实现 agent | 写产品代码：`NlheGame6` / `PluribusActionAbstraction` / `EsMccfrTrainer` Linear+RM+ 路径 / `LbrEvaluator` / `SlumbotBridge` / `Opponent6Max` / `TrainingMetrics` / `Checkpoint v2` 等 | 修改测试代码 |
| **[报告]** | 报告者（人 / 报告 agent） | 跑全套测试、产出验收报告、6-traverser per-traverser metrics 表 + LBR/Slumbot/baseline 实测对照表 | — |

跨界规则、`carve-out` 追认机制、`#[ignore]` full-volume 测试在下一步 [实现] 步骤实跑、CLAUDE.md 同步责任、修订历史"追加不删" — 全部继承 stage 1 + stage 2 + stage 3 §修订历史 提炼的处理政策。**阶段 4 §修订历史 首条新增项必须显式 carry forward 这套政策**，不重新论证。

## 工程脚手架与技术栈选择

### 沿用 Rust（继承 stage 1 + stage 2 + stage 3）

stage 1 + stage 2 + stage 3 已锁定的 dependency 全部继承。stage 4 候选新增依赖（A0 [决策] D-373-rev3 锁定，详见 `pluribus_stage4_api.md` API-499）：

- `rayon = "1"`（显式提到 stage 3 E2-rev1 隐式依赖；stage 4 真并发 ES-MCCFR 必需）
- `reqwest = { version = "0.11", features = ["blocking", "json"] }`（D-463 Slumbot HTTP bridge）
- `serde_json = "1"`（D-474 JSONL log）

**不引入**（与 stage 3 字面继承）：① `nalgebra` / `ndarray`（stage 2 D-250 自实现政策）；② `tokio` / `async-std`（CFR CPU-bound 不需 async）；③ ML 框架（用户路线明确 stage 4-6 全 MCCFR + nested subgame solving，不引 NN）；④ `dashmap` / `parking_lot` / `crossbeam`（stage 3 D-321-rev2 lock 拒绝引入，stage 4 maintain）。

**stage 4 候选新增（A0 lock 不引入，B2 起步前 evaluate D-NNN-revM 翻面）**：

- `fxhash = "0.2"`（D-430-revM FxHashMap 替代 std::HashMap，估计 10-20% throughput 收益）
- `tokio = { version = "1", features = ["rt", "macros"] }`（如 Slumbot HTTP 必须 async；A0 lock blocking 走 `reqwest::blocking`）

### Module 布局（API-498）

stage 4 在 `poker` crate 下加新 module，**不分 crate**（继承 stage 1 D-010..D-012 + stage 2 + stage 3 同型政策）：

```
src/
├── core/                   # stage 1 锁定，stage 4 只读
├── rules/                  # stage 1 锁定，stage 4 只读
├── eval/                   # stage 1 锁定，stage 4 只读
├── history/                # stage 1 锁定，stage 4 只读
├── abstraction/            # stage 2 锁定，stage 4 扩展 PluribusActionAbstraction
│   ├── ...
│   └── action_pluribus.rs  # ★ stage 4 新增（D-420 / API-420）
├── error.rs                # stage 1 + 2 + 3 锁定；stage 4 仅追加 TrainerError 新 variant
└── training/               # stage 3 锁定；stage 4 扩展
    ├── ...                 # stage 3 既有 9 文件
    ├── nlhe_6max.rs        # ★ NlheGame6 (D-410 / API-410)
    ├── lbr.rs              # ★ LbrEvaluator (D-450 / API-450)
    ├── slumbot_eval.rs     # ★ SlumbotBridge (D-460 / API-460)
    ├── baseline_eval.rs    # ★ Opponent6Max + 3 baseline (D-480 / API-480)
    └── metrics.rs          # ★ TrainingMetrics + TrainingAlarm (D-470 / API-470)
```

`tools/`：stage 4 新增

- `lbr_compute.rs` CLI（D-450 / API-452）
- `eval_blueprint.rs` CLI（D-461 Slumbot + D-481 baseline 整合评测）
- `train_cfr.rs` 扩展（D-372 stage 3 既有；stage 4 新增 CLI flag 11 个，API-490）

`tests/` stage 4 新增（候选名）：

- `tests/nlhe_6max_warmup_byte_equal.rs`（B1 [测试] D-409 warm-up phase BLAKE3 byte-equal anchor）
- `tests/nlhe_6max_raise_sizes.rs`（B1 [测试] D-422 14-action raise sizes 走 stage 1 GameState 验证）
- `tests/nlhe_6max_six_traverser.rs`（C1 [测试] D-412 / D-414 6-traverser 隔离）
- `tests/checkpoint_v2_round_trip.rs`（D1 [测试] Checkpoint schema_version=2）
- `tests/training_24h_continuous.rs`（D1 [测试] D-461 24h fuzz）
- `tests/lbr_eval_convergence.rs`（E1 [测试] LBR 单调下降 + < 200 mbb/g first usable）
- `tests/slumbot_eval.rs`（F1 [测试] Slumbot 100K 手 / D-461 + D-462）
- `tests/baseline_eval.rs`（F1 [测试] 1M 手 vs 3 baseline / D-480 + D-481）
- `tests/api_signatures.rs`（每 step 同 PR 扩展 trip-wire）
- `benches/stage4.rs`（D-496 / API-499）

checkpoint artifact 落到 `artifacts/`（继承 stage 2 D-251 + stage 3 模式），**不进 git history**（stage 4 出口 F3 决定 GitHub Release 上传由用户手动触发）。

---

## 步骤序列

总览：`A → B → C → D → E → F`，共 6 个阶段、13 个步骤（与 stage 1 + stage 2 + stage 3 同形态）。每个阶段内部测试与实现交替推进。

```
A. 决策与脚手架                          : A0 [决策] → A1 [实现]
B. 第一轮：warm-up phase 数值连续性 + Linear+RM+ 单元数值  : B1 [测试] → B2 [实现]
C. 第二轮：NlheGame6 6-traverser 14-action + 桥接 stage 1/2  : C1 [测试] → C2 [实现]
D. 第三轮：Checkpoint v2 + 24h continuous + 6-traverser fuzz  : D1 [测试] → D2 [实现]
E. 第四轮：性能 SLO + LBR computation              : E1 [测试] → E2 [实现]
F. 收尾：Slumbot + baseline + 报告                  : F1 [测试] → F2 [实现] → F3 [报告]
```

---

### A. 决策与脚手架

#### 步骤 A0：算法 / API 契约锁定 [决策]

**目标**：锁定 stage 4 全部开放决策点，给后续 [测试] / [实现] agent 一份共同 spec。

**输入**：
- `pluribus_path.md` §阶段 4 字面 8 条门槛
- stage 1 + stage 2 + stage 3 全部决策 + API（D-001..D-103 + D-200..D-283 + D-300..D-379 + API-001..API-013 + API-200..API-302 + API-300..API-392）
- 用户决策：Linear MCCFR + RM+ / Pluribus 14-action / AWS or vultr cloud on-demand
- stage 3 §8.1 carry-forward 7 项（I..VII）— 不阻塞 stage 4 起步，列入 stage 4 主线 13 步 + 并行清单分流

**产出（6 batch）**：
1. `docs/pluribus_stage4_validation.md`（path.md §阶段 4 字面 8 条门槛量化展开 + 通过标准 + 完成产物 + 进入 stage 5 门槛）
2. `docs/pluribus_stage4_decisions.md` §1 算法变体 D-400..D-409（Linear MCCFR + RM+ + warm-up + D-400 详解伪代码）
3. `docs/pluribus_stage4_decisions.md` §2-§5 D-410..D-449（6-player NLHE + Action/Info abstraction + RegretTable 扩展 + Checkpoint v2 schema + first usable / production 双阈值）
4. `docs/pluribus_stage4_decisions.md` §6-§10 D-450..D-499（LBR + Slumbot + 监控 + baseline sanity + SLO + host 选型）
5. `docs/pluribus_stage4_api.md` API-400..API-499（NlheGame6 / PluribusActionAbstraction / EsMccfrTrainer Linear+RM+ / LbrEvaluator / SlumbotBridge / Opponent6Max / TrainingMetrics / Checkpoint v2 + 桥接 + doc-test）
6. `docs/pluribus_stage4_workflow.md`（本文档）+ CLAUDE.md 更新 stage 4 起步状态

**carve-out**：D-401-revM Linear decay eager vs lazy + D-421-revM preflop 独立 action set + D-423-rev0 InfoSet 14-bit mask 区域 + D-430-revM FxHashMap + D-441-rev0 production 触发 + D-447-revM 存储位置 + D-453-revM LBR OpenSpiel fallback + D-463-revM Slumbot fallback 共 8 项 deferred 到后续 step 评估 lock。stage 3 §8.1 carry-forward (I)..(VII) 7 项 carry 入 stage 4 主线 + 并行清单分流处理。

#### 步骤 A1：scaffold 落地 [实现]

**目标**：把 `pluribus_stage4_api.md` 锁定的全部公开 trait / struct / enum 签名落到 `src/training/nlhe_6max.rs` + `src/training/lbr.rs` + `src/training/slumbot_eval.rs` + `src/training/baseline_eval.rs` + `src/training/metrics.rs` + `src/abstraction/action_pluribus.rs` + `tools/lbr_compute.rs` + `tools/eval_blueprint.rs`，方法体 `unimplemented!()` 占位；通过 `cargo build --all-targets` + `cargo fmt --all --check` + `cargo clippy --all-targets -- -D warnings`。

**产出**：
- 5 个新 `src/training/*.rs` + 1 个新 `src/abstraction/*.rs` 共 6 个新 module + stage 3 `src/training/*` 既有 module 扩展（如 `EsMccfrTrainer::with_linear_rm_plus()` builder）
- 2 个新 `tools/*.rs` CLI scaffold
- `Cargo.toml` 追加 rayon / reqwest / serde_json 3 个 crate
- `src/lib.rs` 追加 7 个新 re-export
- `tests/api_signatures.rs` 同 PR 扩展 stage 4 公开 API 签名 trip-wire（~50 行）
- 5 道 gate 全绿（fmt / build / clippy / doc / test --no-run）

**carve-out**：A1 期间允许 ES-MCCFR Linear+RM+ 路径 `unimplemented!()`；B2 [实现] 落地翻面。LBR / Slumbot / baseline / Metrics 各 trait method 全 `unimplemented!()` 占位，F2 [实现] 翻面。

---

### B. 第一轮：warm-up 数值连续性 + Linear+RM+ 单元

#### 步骤 B1：warm-up byte-equal anchor + Linear+RM+ 数值单元测试 [测试]

**目标**：钉死 stage 3 1M update × 3 BLAKE3 anchor 在 stage 4 warm-up phase byte-equal 维持（D-409）+ Linear discounting decay factor / RM+ clamp 数值正确性单元 + D-422 14-action raise sizes 走 stage 1 GameState byte-equal 验证。

**产出测试**：
- `tests/nlhe_6max_warmup_byte_equal.rs`：5 条测试（D-409 warm-up phase 1M update BLAKE3 anchor / warm-up boundary deterministic byte-equal / warm-up → Linear+RM+ 切换数值连续 / Linear weighting 在 t=2 cumulative formula 单元 / RM+ clamp 在 R_t-1 < 0 的 boundary 单元）
- `tests/nlhe_6max_raise_sizes.rs`：14 条测试（每 PluribusAction 一条，走 stage 1 GameState::apply byte-equal regression — D-422）
- `tests/regret_matching_numeric.rs` 扩展（stage 3 既有）：追加 5 条 Linear weighted regret / RM+ clamp 数值容差测试（继承 stage 3 D-330 `1e-9` 容差 + D-403 Linear weighted strategy sum 容差）

**通过标准**：default profile 全 23 测试 panic-fail（`unimplemented!()` scaffold 形态）+ 1 active anchor 测试通过（stage 3 1M update × 3 BLAKE3 anchor 在 stage 4 commit byte-equal 维持 — B1 提前 anchor，B2 实现后转绿）。

**角色边界**：本 batch 0 `src/` 改动 + 0 `tools/` 改动 + 0 `docs/pluribus_stage4_{validation,decisions,api}.md` 改动；触及 `tests/{nlhe_6max_warmup_byte_equal,nlhe_6max_raise_sizes,regret_matching_numeric}.rs` + `docs/pluribus_stage4_workflow.md`（§修订历史 entry）+ CLAUDE.md（## Stage 4 progress section 翻面）。

#### 步骤 B2：Linear MCCFR + RM+ + warm-up 切换 [实现]

**目标**：实现 `EsMccfrTrainer::with_linear_rm_plus()` builder + Linear discounting (D-401) + RM+ clamp (D-402) + warm-up phase (D-409) + 14-action raise sizes 验证 (D-422)。

**产出代码**：
- `src/training/trainer.rs` 扩展：`EsMccfrTrainer::with_linear_rm_plus(warmup_complete_at)` builder + `step()` 内部 warm-up routing + Linear weighting decay eager 路径（D-401 eager 实现）+ RM+ clamp 路径（D-402 in-place update 后 clamp）
- `src/training/regret.rs` 扩展：Linear weighted strategy sum 累积 (D-403)
- `src/abstraction/action_pluribus.rs` 实现：`PluribusActionAbstraction::actions(&GameState)` 走 stage 1 GameState 计算 14 raise legality

**通过标准**：B1 全 23 active 测试转绿（含 stage 3 1M update × 3 BLAKE3 anchor 在 warm-up phase byte-equal 维持锚点）+ stage 3 D-362 anchor `cargo test --release --test cfr_simplified_nlhe -- --ignored` 维持 byte-equal 不退化。

**角色边界**：本 batch 触及 `src/training/{trainer,regret}.rs` + `src/abstraction/action_pluribus.rs` + 文档（§修订历史 + CLAUDE.md）。**0 改动**：`tests/*`（继承 stage 1 / 2 / 3 角色边界政策）+ `docs/pluribus_stage4_{validation,decisions,api}.md`（B2 [实现] 不修改决策 / API 文档；如发现 spec 错误走 D-NNN-revM 流程）。

**B2 → C1 工程契约**：D-401 eager decay 路径性能开销估计 ~30% 单线程，B2 实测 throughput 落地。若实测 < D-490 单线程 5K update/s SLO → D-401-revM lazy decay 翻面 evaluate（C2 [实现] 起步前 lock）。

---

### C. 第二轮：NlheGame6 6-traverser 14-action 接入

#### 步骤 C1：NlheGame6 + 6-traverser + 14-action InfoSet bit 测试 [测试]

**目标**：钉死 `NlheGame6` Game trait impl 6-player 路径 + 6-traverser 独立 RegretTable + 14-action InfoSet bit mask (D-423) + stage 1 `GameState` n_seats=6 路径 byte-equal 维持 (D-415)。

**产出测试**：
- `tests/nlhe_6max_six_traverser.rs`：6 条测试（D-411 NlheGame6 root state n_seats=6 / D-412 alternating traverser routing / D-414 6-traverser 不共享 / D-415 stage 1 GameState n_seats=6 路径 byte-equal regression / D-416 HU 退化路径 with `new_hu()` byte-equal stage 3 SimplifiedNlheGame anchor / D-418 6-player side pot fuzz）
- `tests/nlhe_6max_infoset_mask.rs`：4 条测试（D-423 InfoSetId bits 33..47 14-bit mask 编码解码 / mask boundary 14 vs 5 action ablation / mask collision 检测 / stage 2 InfoSetId 64-bit layout 字面边界 regression）
- `tests/api_signatures.rs` 扩展：API-410..API-429 trip-wire（NlheGame6 / PluribusActionAbstraction / Game::VARIANT::Nlhe6Max / GameVariant::from_u8(3)）

**通过标准**：default profile 12 测试 panic-fail（C2 实现后转绿）+ 4 active anchor 测试通过（stage 1 `GameState` n_seats=6 byte-equal 维持 + stage 3 SimplifiedNlheGame anchor 不退化）。

**角色边界**：本 batch 0 `src/` 改动；触及 `tests/{nlhe_6max_six_traverser,nlhe_6max_infoset_mask,api_signatures}.rs` + 文档。

#### 步骤 C2：NlheGame6 + 6-traverser + 14-action InfoSet bit 实现 [实现]

**目标**：实现 `NlheGame6` impl Game trait 全 8 方法 + 6 套独立 RegretTable 数组 + InfoSetId 14-bit mask (D-423) + HU 退化路径 (D-416)。

**产出代码**：
- `src/training/nlhe_6max.rs`：`NlheGame6` struct + impl Game trait + `traverser_at_iter` / `traverser_for_thread` 6-traverser routing
- `src/training/trainer.rs` 扩展：`EsMccfrTrainer<NlheGame6>::step` + `step_parallel` 走 6-traverser alternating + 6 套独立 RegretTable 数组 (`[RegretTable<NlheGame6>; 6]`)
- `src/abstraction/abstraction.rs` 扩展（候选名）：`InfoSetId::with_14action_mask` + `legal_actions_mask_14` 接口
- 继承 stage 3 D-321-rev2 rayon thread pool + append-only delta merge 模式扩展到 6-traverser

**通过标准**：C1 全 10 active 测试转绿（含 6-traverser 独立 + 14-bit mask + HU 退化路径 byte-equal stage 3 anchor）+ stage 1 + 2 + 3 baseline 不退化（`stage{1,2,3}-v1.0` tag 在 C2 commit 上仍可重跑 byte-equal）。

**C2 → D1 工程契约**：C2 commit ship serial-equivalent + rayon thread pool；6-traverser path 数值正确性（含 14-action raise size in stage 1 GameState 路径 byte-equal）在 C2 commit 后 anchor，D1 [测试] 钉死 checkpoint round-trip 不变量。

**角色边界**：本 batch 触及 `src/training/{nlhe_6max,trainer}.rs` + `src/abstraction/*` + 文档；**0 改动** `tests/*` + `docs/pluribus_stage4_{validation,decisions,api}.md`（C2 [实现] 不修改决策 / API 文档）。

---

### D. 第三轮：Checkpoint v2 + 24h continuous + 6-traverser fuzz

#### 步骤 D1：Checkpoint v2 round-trip + 24h fuzz + corruption 测试 [测试]

**目标**：钉死 Checkpoint schema_version=2 round-trip BLAKE3 byte-equal (D-445) + 跨版本 schema_version=1 ↔ 2 拒绝路径 + 24h continuous run 无 panic/OOM/NaN (D-461) + 6-traverser checkpoint snapshot 不退化。

**产出测试**：
- `tests/checkpoint_v2_round_trip.rs`：18 条测试（schema_version 2 round-trip 6-traverser × 14-action / 跨版本 1 → 2 SchemaMismatch / 2 → 1 SchemaMismatch / linear_weighting_enabled mismatch 拒绝 / rm_plus_enabled mismatch 拒绝 / warmup_complete=0 → 1 边界恢复 / traverser_count=1 → 6 不兼容 / 5 类 CheckpointError v2 byte-flip / 128-byte header 字段 layout 常量 lock / atomic write tempfile fail 路径 / 6 traverser regret_table BLAKE3 byte-equal / 6 traverser strategy_sum BLAKE3 byte-equal / 6 traverser cross-load equivalence / regret_offset + strategy_offset 计算正确 / bucket_table_blake3 mismatch / 等）
- `tests/training_24h_continuous.rs`：3 条测试（`#[ignore]` opt-in，release profile）：stage4_six_max_24h_no_crash / RSS 增量 < 5 GB / 每 10⁸ update checkpoint 写入成功）
- `tests/cfr_fuzz.rs` 扩展（stage 3 既有）：追加 6 条 6-player + 14-action + Linear+RM+ 路径 fuzz 测试

**通过标准**：default profile 27 测试（18 + 6 fuzz + 3 ignored）panic-fail（D2 实现后转绿）+ 1 active anchor（128-byte header layout 常量 + GameVariant::from_u8(3) trip-wire）。

#### 步骤 D2：Checkpoint v2 + 24h continuous run 实现 [实现]

**目标**：实现 `Checkpoint::save_v2` / `Checkpoint::open` schema v2 dispatch + 6-traverser bincode 序列化 + `EsMccfrTrainer::save_checkpoint` / `load_checkpoint` 6-traverser 路径 + 24h continuous run wrapper （`tests/training_24h_continuous.rs` 配套实现）。

**产出代码**：
- `src/training/checkpoint.rs` 扩展：128-byte v2 header + bincode body + 6-traverser dispatch + schema_version 1 vs 2 路径分流 + `TrainerVariant::ESMccfrLinearRmPlus` 新增
- `src/training/trainer.rs` 扩展：`EsMccfrTrainer<NlheGame6>::save_checkpoint` 6 套 RegretTable 序列化 + load 路径
- `src/error.rs` 扩展：`TrainerError` 新增 `OutOfMemory { rss_bytes, limit }` variant（D-431 RSS 监控 P0 阻塞）

**通过标准**：D1 全 24 active 测试 + 3 ignored 测试翻绿 release `--ignored` 实测 D-461 24h 通过 + stage 3 D1 [测试] 全套测试 byte-equal 不退化（D-445 BLAKE3 round-trip 6-traverser 扩展）。

**D2 → E1 工程契约**：D2 实现 `Checkpoint::save_v2` 后 24h continuous run 实测落地。若实测 < 24h 触发 OOM → D-431 阈值翻面 D-431-revM evaluate 或 host 升级（c7a.4xlarge → c7a.8xlarge）。

---

### E. 第四轮：性能 SLO + LBR computation

#### 步骤 E1：性能 SLO + LBR 收敛 [测试]

**目标**：钉死 stage 4 性能 SLO（D-490 单线程 5K / 4-core 15K / 32-vCPU 20K update/s） + LBR exploitability 单调下降 + LBR computation P95 < 30 s + 24h continuous run wall-time SLO。

**产出测试**：
- `tests/perf_slo.rs::stage4_*`：8 条 SLO 测试（`#[ignore]` opt-in，release profile + AWS / vultr 实测触发）：D-490 ① 单线程 5K / ② 4-core 15K / ③ 32-vCPU 20K / D-454 LBR P95 30s / D-485 baseline eval 2min / D-461 24h continuous wall-time / D-498 7-day nightly fuzz / D-490 6-traverser per-traverser throughput cross-check
- `tests/lbr_eval_convergence.rs`：6 条测试（first usable 10⁹ update 后 LBR < 200 mbb/g / LBR 100 采样点单调非升 ±10% / LBR per-traverser 上界 < 500 mbb/g D-459 / OpenSpiel-export policy 文件 byte-equal / 14-action LBR enumerate 范围正确 / D-455 myopic horizon=1 边界）

**通过标准**：default profile 14 测试 panic-fail（E2 实现后转绿）+ `--ignored` 触发 4 SLO 在 AWS c7a 实测 first usable training 完成后达到 D-490 阈值。

#### 步骤 E2：性能优化 + LBR Rust 自实现 [实现]

**目标**：性能优化（HashMap → FxHashMap 评估、Linear decay lazy 评估 D-401-revM、6-traverser thread pool 优化）+ `LbrEvaluator` Rust 自实现（D-453）+ `tools/lbr_compute.rs` CLI 主体。

**产出代码**：
- `src/training/lbr.rs`：`LbrEvaluator::compute` + `compute_six_traverser_average` + `export_policy_for_openspiel` 全 trait method 实现
- `src/training/trainer.rs` 扩展：根据 E1 实测决定是否走 D-401-revM lazy decay / D-430-revM FxHashMap 翻面
- `tools/lbr_compute.rs`：CLI 主体落地（D-450 / API-452）

**通过标准**：E1 全 14 active 测试转绿 + AWS c7a.8xlarge 实测 D-490 32-vCPU 20K update/s + D-454 LBR P95 30s。

**E2 → F1 工程契约**：E2 commit ship D-490 性能 SLO 实测翻面。如 first usable 10⁹ update 后 LBR ≥ 200 mbb/g → D-421-revM preflop 独立 action set 翻面 evaluate（F2 起步前 + 用户授权 lock）。

---

### F. 收尾

#### 步骤 F1：Slumbot + baseline + corner case 测试 [测试]

**目标**：钉死 Slumbot 100K 手评测协议 + 1M 手 vs 3 类 baseline + corner case + cross-host BLAKE3 baseline。

**产出测试**：
- `tests/slumbot_eval.rs`：6 条测试（`#[ignore]` opt-in + Slumbot API 在线）：100K 手 mean ≥ -10 mbb/g first usable / 95% CI 下界 ≥ -30 mbb/g / duplicate dealing on/off ablation / 5 次重复 mean / fold equity sanity / Slumbot API 不可用时 D-463-revM fallback OpenSpiel HU baseline）
- `tests/baseline_eval.rs`：12 条测试（3 baseline × 4 metric：random ≥ +500 / call-station ≥ +200 / TAG ≥ +50 / per-traverser min ≥ floor / 1M 手 BLAKE3 byte-equal regression / 95% CI 下界 > 0）
- `tests/cross_host_blake3.rs` 扩展（stage 3 既有）：追加 stage 4 baseline `tests/data/checkpoint-hashes-linux-x86_64-stage4.txt` 32-seed × 6-traverser × first usable 10⁵ checkpoint anchor
- `tests/api_signatures.rs` 扩展：API-450..API-499 全 trip-wire（LBR / Slumbot / baseline / metrics / 24h continuous）

**通过标准**：default profile 30 测试 panic-fail（F2 实现后转绿）+ 8 active anchor 测试通过（Cross-host baseline 32-seed × stage 4 不退化 + api_signatures 全套）。

#### 步骤 F2：Slumbot bridge + baseline opponents + 监控告警 实现 [实现]

**目标**：实现 `SlumbotBridge` HTTP client + `Opponent6Max` trait + 3 baseline impl + `TrainingMetrics` 监控 + `tools/eval_blueprint.rs` CLI。

**产出代码**：
- `src/training/slumbot_eval.rs`：`SlumbotBridge::play_one_hand` + `evaluate_blueprint` 100K 手 + duplicate dealing + 5 次重复
- `src/training/baseline_eval.rs`：`Opponent6Max` trait + `RandomOpponent` + `CallStationOpponent` + `TagOpponent` 3 impl + `evaluate_vs_baseline` 1M 手
- `src/training/metrics.rs`：`MetricsCollector::observe` + JSONL log + `TrainingAlarm` 5 variant dispatch
- `tools/eval_blueprint.rs`：CLI 主体（Slumbot + baseline 整合评测，D-461 / D-481）

**通过标准**：F1 全 30 active 测试转绿 + `cargo test --no-fail-fast` 默认 profile 不退化 + stage 1 + 2 + 3 baseline 不退化。

**F2 → F3 工程契约**：F2 commit 后 stage 4 first usable `10⁹` update 训练由用户授权 D-491 AWS c7a.8xlarge 启动（wall-time ~14h + $20 cost）；F3 起草 wait for first usable 训练完成 + LBR / Slumbot / baseline 评测落地。

#### 步骤 F3：验收报告 [报告]

**目标**：产出 stage 4 验收报告 + git tag `stage4-v1.0` + first usable blueprint artifact 上传 GitHub Release（由用户手动触发）。

**产出文档**：
- `docs/pluribus_stage4_report.md`：含 first usable `10⁹` update 训练曲线 / LBR 100 采样点（per-traverser × 6 + 6-traverser average）/ Slumbot 5×100K 手实测 / baseline 3 类 × 3 seed 实测 / 性能 SLO 实测值（vultr 4-core + AWS c7a.8xlarge × 32 vCPU）/ D-491 host 选型 + 时间预算实测 / 关键 seed 列表 / 版本哈希 / 已知偏离 + stage 5 起步并行清单
- `docs/pluribus_stage4_external_compare.md`：OpenSpiel LBR Python reference 对照（D-457 一次性 sanity）+ Slumbot HU 公开评测数据对照（D-469 fold equity 校验）
- `tools/checkpoint_reader.py` 扩展（stage 3 既有）：支持 Checkpoint v2 schema 128-byte header + 6-traverser 解析
- git tag `stage4-v1.0`（commit message 含完整 carve-out 索引）
- first usable blueprint checkpoint artifact 上传 GitHub Release tag `stage4-v1.0`（30-50 GB checkpoint，由用户手动触发 `gh release upload`）

**carve-out 状态翻面**：
- production `10¹¹` 训练：F3 [报告] 闭合后由用户授权 D-441-rev0 启动；wall-time ~58 days AWS c7a.8xlarge $4600 cost，作为 stage 5 起步并行清单 carry-forward 项
- stage 3 §8.1 carry-forward (I)..(VII) 7 项：carry-forward 状态在 F3 报告 §carve-out 索引落地，stage 4 主线 13 步分流处理结果作为已知偏离 / 已完成 / 部分完成入档
- 12 条 `tests/bucket_quality.rs` `#[ignore]` 转 active：stage 4 F1 后视情况翻面或继续 carry-forward 到 stage 5 起步并行清单
- AIVAT / DIVAT 方差缩减接口（path.md §阶段 7 字面 stage 7 依赖）：stage 4 F3 不阻塞 stage 7 起步该项 carve-out
- D-441-rev0 production blueprint 训练 host budget + timing approval：carry-forward stage 5 起步并行清单

---

## 反模式（不要做）

继承 stage 1 + stage 2 + stage 3 反模式全集，**额外强调 stage 4 高风险反模式**：

1. **优化前确认正确性**——E2 之前任何 Linear weighting / RM+ clamp / 6-traverser routing 性能优化都是错误时序。先 B2/C2/D2 把数值 anchor 钉死，再 E2 推优化。
2. **跳过 stage 3 1M update × 3 BLAKE3 anchor**——B1 [测试] 必须把 warm-up phase byte-equal 钉死。stage 3 anchor 是 stage 4 数值正确性唯一继承锚点；warm-up phase 漂移 = Linear / RM+ 实现错位 silent bug 渗入 stage 5-6 search。
3. **隐式 RNG**——stage 4 训练循环 + LBR computation + Slumbot evaluation + baseline opponent 任何 sampling / tie-break / shuffle 必须显式接 `RngSource`。任何 `rand::thread_rng()` 是 P0 阻塞 bug，违反 stage 1 D-027 + D-050。
4. **f32 替代 f64 / 跨 stage 改 stage 1 GameState::apply**——D-433 lock f64；D-422 stage 1 字面禁止改。需要时走 D-NNN-revM + 用户授权 + stage 1 / 2 / 3 测试套件 byte-equal 维持。
5. **NN 替代 MCCFR**——用户路线 memory `project_stage4_6_path.md` 字面 stage 4-6 走纯 MCCFR + 实时 nested subgame solving，不引入 DeepCFR / ReBeL / 神经网络变体。任何 NN 替换 regret 估计 / abstraction 的 PR 默认拒绝（继承 memory feedback）。
6. **bucket table mid-training 升级**——D-429 lock；stage 4 训练全程 v3 production artifact (BLAKE3 `67ee5554...`) 固定。任何 bucket table 升级（D-218-rev3 真等价类 v4 等）需要 fresh start training，不允许 hot-swap。
7. **production `10¹¹` 训练绕过用户授权**——D-441 lock production 训练 deferred 到 F3 闭合后 + 用户书面授权 + D-441-rev0；wall-time 58 days × $4600 cost 不允许 stage 4 主线自动触发。CLAUDE.md memory `feedback_high_perf_host_on_demand.md` 字面 "> 1h wall time 任务必须用户授权"。
8. **LBR P0 阻塞替代验收锚点**——D-450 lock LBR 上界 + Slumbot + baseline + 监控四条**独立弱锚点**；不允许用单一锚点（如只 LBR）替代四条独立锚点。一条单独通过不足以说明 blueprint 实战质量。
9. **OpenSpiel 数值 byte-equal**——D-457 lock OpenSpiel LBR mbb/g 差异 `< 10%` 容差；任何强求 byte-equal 是 stage 4 工程红线（OpenSpiel 实现细节 + myopic horizon + sampling 不同导致 silent 差异）。
10. **24h continuous run 静默 OOM 通过**——D-461 lock RSS 增量 `< 5 GB`。RSS 监控走 trainer `metrics()` 返回路径，trainer 不主动 abort；但 CLI 必须 abort-on-alarm p0 触发立即退出，**不允许**静默吸收 OOM 信号让训练继续到 OOM-kill。

---

## 阶段 4 出口检查清单

只有当全部门槛全部满足，才能 git tag `stage4-v1.0`：

- [ ] `cargo test`（默认）全套通过；ignored 测试数 stage 4 新增 ≤ 20（与 stage 3 +14 比例放宽 ~1.4× 反映 stage 4 LBR/Slumbot/baseline/24h 多个 release-only 评测套件）
- [ ] `cargo test --release -- --ignored` 全套通过；含 Checkpoint v2 round-trip 6-traverser + 24h continuous + 6 trainer cross-traverser
- [ ] `cargo test --release --test perf_slo -- --ignored --nocapture stage4_` 全部 SLO 实测达到 D-490..D-499 阈值（单线程 5K / 4-core 15K / 32-vCPU 20K / LBR P95 30s / baseline 2min / 24h continuous + 7-day fuzz）
- [ ] `cargo bench --bench stage4` 3 个 bench group active throughput 数据完整
- [ ] `cargo fmt --all --check` / `cargo clippy --all-targets -- -D warnings` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 全绿
- [ ] `tests/api_signatures.rs` 覆盖 stage 4 全部公开 API surface，0 签名漂移
- [ ] stage 1 baseline 不退化（`stage1-v1.0` tag 在 stage 4 任何 commit 上仍可重跑 byte-equal）
- [ ] stage 2 baseline 不退化（`stage2-v1.0` tag 在 stage 4 任何 commit 上仍可重跑 byte-equal）
- [ ] stage 3 baseline 不退化（`stage3-v1.0` tag 在 stage 4 任何 commit 上仍可重跑 byte-equal）— stage 3 1M update × 3 BLAKE3 anchor + Kuhn closed-form + Leduc fixed-seed 等 stage 3 出口实测全部 byte-equal
- [ ] **first usable `10⁹` update 训练完成**：6-traverser 完整 first usable blueprint artifact 生成 + checkpoint round-trip BLAKE3 byte-equal + 6 per-traverser metrics 落地
- [ ] **LBR < 200 mbb/g first usable** + per-traverser min < 500 mbb/g + 100 采样点单调非升 ±10%
- [ ] **Slumbot 100K 手 mean ≥ -10 mbb/g first usable** + 95% CI 下界 ≥ -30 mbb/g + 5 次重复 standard error
- [ ] **1M 手 vs 3 baseline 全过**：random ≥ +500 / call-station ≥ +200 / TAG ≥ +50 mbb/g（95% CI 下界 > 0）
- [ ] **24h continuous run 无 panic / NaN / inf** + RSS 增量 `< 5 GB` + 全部 checkpoint round-trip BLAKE3 byte-equal
- [ ] **多人 CFR 监控**：average regret growth sublinear + entropy 单调下降 + 动作概率震荡幅度单调下降；3 条曲线任意连续 5 个采样点违反趋势 → trainer 告警；F3 报告输出 30K data point 监控曲线
- [ ] **EV 零和约束**：6-traverser EV sum residual `< 1e-3 mbb/g`（D-478 字面）
- [ ] OpenSpiel LBR mbb/g 对照差异 `< 10%`（D-457 字面，F3 一次性 sanity）
- [ ] `docs/pluribus_stage4_report.md` 落地 + git tag `stage4-v1.0` + first usable blueprint artifact 上传 GitHub Release（由用户手动触发）

---

## 时间预算汇总

按 `pluribus_path.md` §阶段 4 字面 `3-6 人月（含训练等待时间）` 估算：

| 步骤 | 时间预算 | 单 host 实测预期 |
|---|---|---|
| A0 [决策] | 0.5 周（6 batch commit） | 文档起草 + review |
| A1 [实现] scaffold | 0.5 周 | 6 新 module + 2 CLI scaffold + Cargo.toml + lib.rs |
| B1 [测试] warm-up + Linear/RM+ 单元 | 0.7 周 | 23 测试 + stage 3 anchor regression 钉死 |
| B2 [实现] Linear MCCFR + RM+ + warm-up | 1.0 周 | trainer Linear / RM+ / warm-up 路径 + Pluribus 14-action |
| C1 [测试] NlheGame6 + 6-traverser + InfoSet | 0.7 周 | 16 测试 + stage 1 GameState n_seats=6 byte-equal |
| C2 [实现] NlheGame6 + 6-traverser + 14-bit mask | 1.0 周 | Game trait 8 method + 6 套独立 RegretTable + HU 退化 |
| D1 [测试] Checkpoint v2 + 24h + fuzz | 0.7 周 | 27 测试 + 24h continuous run wrapper |
| D2 [实现] Checkpoint v2 + 6-traverser save/load | 1.0 周 | 128-byte header + bincode body + dispatch 路径 |
| E1 [测试] perf SLO + LBR convergence | 0.5 周 | 14 测试 + AWS c7a.8xlarge 实测预备 |
| E2 [实现] perf opt + LBR Rust 自实现 | 1.5 周 | 性能优化 + LBR computation + AWS c7a 实测落地 |
| F1 [测试] Slumbot + baseline + corner | 0.7 周 | 30 测试 + Slumbot API 在线验证 |
| F2 [实现] Slumbot bridge + 3 baseline + 监控 | 1.0 周 | bridge + 3 baseline + metrics collector + JSONL log |
| F3 [报告] | 1.0 周 | first usable 10⁹ 训练 14h + LBR / Slumbot / baseline 评测 + 报告 |
| **总计** | **~10.8 周** | path.md 3-6 人月 (12-26 周) 范围内，path.md 字面下界 12 周 buffer 1.2 周 |

stage 4 first usable 10⁹ update 训练在 AWS c7a.8xlarge 上实测 ~14 h（D-490 SLO 20K update/s × 10⁹ = 5 × 10⁴ s ≈ 14 h），重复多次（不同 seed × 评测重跑 × Slumbot 100K 手 × baseline 1M 手）共需 ~5-7 days AWS / vultr cost ~$200-500 total（不含 production 10¹¹ deferred 训练 ~$4600）。

stage 3 § 8.1 carry-forward 7 项的实施分流：
- (I) perf flamegraph hot path 实测 → 在 E1/E2 主线落地（E2 [实现] 性能优化前置工作）
- (II) outcome vs external sampling 评估 → A0 [决策] D-405 评估结论维持 external（不阻塞 stage 4 主线）
- (III) stage 1 `GameState::apply` micro-opt → 主线不引入；在 F3 后 + 用户授权 + stage 1 D-NNN-revM 流程评估
- (IV) stage 3 D-361-revM 阈值翻面 → 不直接翻面；stage 4 D-490 替代锁定新阈值
- (V) stage 3 D-362 100M anchor 恢复 → 主线不引入；F3 后 + 用户授权评估
- (VI) stage 2 bucket quality 12 条 #[ignore] 转 active → F1 后视情况翻面或继续 carry-forward 到 stage 5
- (VII) stage 2 `pluribus_stage2_report.md` §8 carve-out 翻面 → F3 后 + 用户授权评估

---

## 参考资料

- `pluribus_path.md` §阶段 4 — 8 条门槛量化定义
- `pluribus_stage4_validation.md` — 量化验收 + 通过标准
- `pluribus_stage4_decisions.md` — D-400..D-499 全决策
- `pluribus_stage4_api.md` — API-400..API-499 全 API surface
- `pluribus_stage1_workflow.md` / `pluribus_stage2_workflow.md` / `pluribus_stage3_workflow.md` — Agent 分工 + carve-out 模式继承
- Brown & Sandholm 2019 (Linear CFR) / Tammelin 2015 (RM+) / Lisý & Bowling 2017 (LBR) — 算法定义参考
- Pluribus 主论文 + 补充材料 §S2 (training) / §S3 (abstraction) / §S5 (cost) — 实战参考

---

## 修订历史

本文档遵循与 `pluribus_stage1_workflow.md` / `pluribus_stage2_workflow.md` / `pluribus_stage3_workflow.md` 相同的"追加不删"约定。

阶段 4 实施期间的角色边界 carve-out 追认（B-rev / C-rev / D-rev / E-rev / F-rev 命名风格继承 stage 1 + stage 2 + stage 3）将在 stage 4 实施期间按 commit-by-commit 落到本节。

- **2026-05-14（A0 [决策] 起步 batch 6 落地）**：stage 4 A0 [决策] 起步 batch 6 落地 `docs/pluribus_stage4_workflow.md`（本文档）+ CLAUDE.md "stage 4 A0 起步 batch 1..6 closed" 状态翻面。本节首条由 stage 4 A0 [决策] batch 6 commit 落地，与 `pluribus_stage4_validation.md` §修订历史 + `pluribus_stage4_decisions.md` §修订历史 + `pluribus_stage4_api.md` §修订历史 同步。
    - §文档目标 + §总体原则 + §Agent 分工：carry forward stage 1 + stage 2 + stage 3 完整政策（角色边界、carve-out 追认、`#[ignore]` 实跑、CLAUDE.md 同步、修订历史追加不删），不重新论证。
    - §工程脚手架与技术栈选择：D-373-rev3 锁定新增 3 个 crate（rayon + reqwest + serde_json）；6 个新 module 布局（`nlhe_6max` / `lbr` / `slumbot_eval` / `baseline_eval` / `metrics` 在 `src/training/`；`action_pluribus` 在 `src/abstraction/`）；`tools/` 新增 2 个（lbr_compute.rs + eval_blueprint.rs）。
    - §步骤序列：13 步 A0 → A1 → B1 → B2 → C1 → C2 → D1 → D2 → E1 → E2 → F1 → F2 → F3，每步含产出 + 不变量 + 测试通过条件 + 性能预期 + B2/C2/D2/E2/F2 → 下一步工程契约。
    - §反模式：10 条 stage 4 特有反模式（warm-up phase byte-equal anchor 必钉死 / f64 不替代 f32 / OpenSpiel LBR 不强求 byte-equal / bucket table mid-training 不升级 / production 训练绕过用户授权 / LBR 单独不替代四锚点验收 等）。
    - §出口检查清单：17 条门槛（含 stage 1 + 2 + 3 baseline 不退化 + first usable 10⁹ 训练完成 + LBR / Slumbot / baseline 三轨实测 + 24h continuous + 多人 CFR 监控 + EV 零和 + OpenSpiel sanity）。
    - §时间预算：~10.8 周（path.md 字面 3-6 人月下界 12 周 buffer 1.2 周；first usable 10⁹ 训练实测 ~14h AWS c7a.8xlarge × $20 cost；不含 production 10¹¹ deferred $4600 cost）。stage 3 §8.1 carry-forward 7 项分流安排（I 在 E1/E2 主线 / II A0 评估结论 / III-VII F3 后用户授权评估）。
    - **Carve-out carry-forward**：stage 3 §8.1 (I)..(VII) 7 项继承到 stage 4 不阻塞起步；A0 [决策] batch 2-4 / batch 5 期间 D-401-revM / D-421-revM / D-423-rev0 / D-430-revM / D-441-rev0 / D-447-revM / D-453-revM / D-463-revM / D-409-revM 共 9 项 deferred 决策记入 `pluribus_stage4_decisions.md` §12 已知未决项 + 各步骤工程契约 lock 时间窗。

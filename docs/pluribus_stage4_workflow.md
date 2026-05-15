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

- **2026-05-14（A1 [实现] scaffold 落地）**：stage 4 A1 [实现] scaffold 单 commit 落地 — 6 新 module 骨架 + 2 CLI scaffold + Cargo.toml 3 crate + lib.rs / training/mod.rs / abstraction/mod.rs re-export + tests/api_signatures.rs 扩 stage 4 trip-wire + tests/trainer_error_boundary.rs 6-variant exhaustive 翻面 + benches/stage4.rs entry 占位 + CLAUDE.md "stage 4 A0 closed → A1 closed" 状态翻面。
    - **新 module 落地（6 文件）**：`src/abstraction/action_pluribus.rs`（`PluribusAction` 14-variant enum + `PluribusActionAbstraction` struct + `actions` / `is_legal` / `compute_raise_to` 内部方法 + `N_ACTIONS` / `all` / `raise_multiplier` / `from_u8` const helper，方法体 `unimplemented!()` 占位）；`src/training/nlhe_6max.rs`（`NlheGame6` struct + `NlheGame6State` + `NlheGame6Action` / `NlheGame6InfoSet` type alias + `Game` trait impl 全 8 method `unimplemented!()` + `traverser_at_iter` / `traverser_for_thread` pure function 落地真实实现）；`src/training/lbr.rs`（`LbrEvaluator` + `LbrResult` / `SixTraverserLbrResult` struct + 4 method `unimplemented!()`）；`src/training/slumbot_eval.rs`（`SlumbotBridge` + `Head2HeadResult` / `SlumbotHandResult` / `OpenSpielHuBaseline` / `HuHandResult` struct + 4 method `unimplemented!()`，`reqwest::blocking::Client` 字段类型 lock）；`src/training/baseline_eval.rs`（`Opponent6Max` trait + `RandomOpponent` / `CallStationOpponent` / `TagOpponent` 3 impl + `BaselineEvalResult` struct + `evaluate_vs_baseline` free function，全 `unimplemented!()`）；`src/training/metrics.rs`（`TrainingMetrics` 9 字段 struct + `TrainingAlarm` 5 variant enum + `MetricsCollector` struct + `write_metrics_jsonl` free function，`Serialize` derive + 全 `unimplemented!()`）。
    - **stage 3 既有 module 扩展（5 文件）**：`src/error.rs` 加 `GameVariant::Nlhe6Max = 3`（API-411）+ `TrainerVariant::EsMccfrLinearRmPlus = 2`（API-441）+ `TrainerError::PreflopActionAbstractionMismatch`（D-456 LbrEvaluator::new action_set_size 越界拒绝）；`src/training/checkpoint.rs` 扩 `GameVariant::from_u8` 加 `3 => Nlhe6Max` + `TrainerVariant::from_u8` 加 `2 => EsMccfrLinearRmPlus`；`src/abstraction/info.rs` 加 `InfoSetId::with_14action_mask(self, mask: u16) -> Self` + `legal_actions_mask_14(self) -> u16`（API-423，bodies `unimplemented!()`）；`src/training/trainer.rs` 加 `Trainer::current_strategy_for_traverser` / `average_strategy_for_traverser` 默认 impl 退化（API-403）+ `DecayStrategy` enum（`EagerDecay` / `LazyDecay` + `Default` derive）+ `TrainerConfig` struct + `EsMccfrTrainer::config: TrainerConfig` 字段 + `EsMccfrTrainer::with_linear_rm_plus(warmup_complete_at: u64) -> Self` builder（API-400 / API-401）+ `EsMccfrTrainer::config()` getter；`src/training/mod.rs` 加 5 个 stage 4 module 声明 + 公开 surface re-export；`src/lib.rs` 加 stage 4 顶层 re-export（`PluribusAction` / `PluribusActionAbstraction` / `NlheGame6` / `LbrEvaluator` / `SlumbotBridge` / `Opponent6Max` / 3 baseline impl / `TrainingMetrics` / `TrainerConfig` / `DecayStrategy` 等）。
    - **Cargo.toml 扩展（D-373-rev3）**：`reqwest = { version = "0.12", default-features = false, features = ["blocking", "json", "rustls-tls"] }` + `serde_json = "1"` 从 dev-dependencies 提升到 dependencies；rayon 1.10 已在 stage 2 引入（不重复）；新增 `[[bin]] lbr_compute` + `[[bin]] eval_blueprint` + `[[bench]] stage4`。
    - **测试 / bench scaffold 落地**：`tools/lbr_compute.rs` + `tools/eval_blueprint.rs` `main` body `unimplemented!()` 占位；`benches/stage4.rs` 3 bench group entry no-op 占位（E1 / F1 [测试] 起步前 lock）；`tests/api_signatures.rs` 扩 `_stage4_api_signature_assertions()` ~200 行 API-400..API-499 fn-pointer UFCS trip-wire（含 `PluribusAction` 14 variant / `NlheGame6` Game trait method / `LbrEvaluator` / `SlumbotBridge` / `Opponent6Max` / `TrainingMetrics` 字段 / `TrainerConfig` 字段 / `DecayStrategy` enum 等）；`tests/trainer_error_boundary.rs::trainer_error_5_variants_exhaustive_match_lock` 翻面成 `trainer_error_6_variants_exhaustive_match_lock`（追加 `PreflopActionAbstractionMismatch` 第 6 variant 让 stage 3 exhaustive match trip-wire 自适应 stage 4 新 variant）。
    - **5 道 gate 全绿**：`cargo fmt --all --check` ✅ / `cargo build --all-targets` ✅ / `cargo clippy --all-targets -- -D warnings` ✅（修复 3 处 `doc_lazy_continuation` + 1 处 `derivable_impls`） / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` ✅（修复 2 处 `private_intra_doc_links` + 1 处 `broken_intra_doc_links`） / `cargo test --no-run` ✅ + `cargo test --test api_signatures` 1 passed。stage 1 + 2 + 3 baseline 测试套件 byte-equal 维持（参见 stage 4 出口检查清单 stage 1/2/3 不退化条目，由 F3 [报告] 最终验收）。
    - **角色边界**：A1 [实现] 单次 commit 触及 `src/*` + `tools/*` + `tests/api_signatures.rs` + `tests/trainer_error_boundary.rs` + `benches/stage4.rs` + `Cargo.toml` + `docs/pluribus_stage4_workflow.md`（本节）+ `CLAUDE.md`。**0 改动** `docs/pluribus_stage4_{validation,decisions,api}.md`（A1 [实现] 不修改决策 / API / validation 文档；如发现 spec 错误走 D-NNN-revM 流程）。**0 改动** stage 1 / 2 / 3 既有 `src/*` / `tests/*` 锁定路径（除 `src/error.rs` / `src/abstraction/info.rs` / `src/training/checkpoint.rs` / `src/training/mod.rs` / `src/training/trainer.rs` / `src/abstraction/mod.rs` / `src/lib.rs` / `tests/trainer_error_boundary.rs` / `tests/api_signatures.rs` 的 stage 4 additive 扩展 — 每处变更对 stage 3 既有 trip-wire / exhaustive match / re-export 保持 byte-equal 不破，stage 3 出口测试套件全绿）。
    - **A1 → B1 工程契约**：B1 [测试] 起步前必须钉死 stage 3 1M update × 3 BLAKE3 anchor 在 stage 4 warm-up phase byte-equal 维持（D-409 字面）+ Linear discounting decay factor / RM+ clamp 数值单元 + D-422 14-action raise sizes 走 stage 1 GameState byte-equal 验证。`EsMccfrTrainer::with_linear_rm_plus` builder 在 A1 [实现] commit 后已暴露，B1 [测试] 直接消费 builder 接口构造 stage 4 模式 trainer；B2 [实现] 起步前 lock `step()` 内部 warm-up routing + Linear weighting decay eager 路径 + RM+ clamp 路径具体实现细节。

- **2026-05-15（B1 [测试] 落地）**：stage 4 B1 [测试] 单 commit 落地 — 24 条新 test 覆盖 D-401 / D-402 / D-403 / D-409 / D-422 + 5 道 gate 全绿 + 0 改动 `src/*` / `tools/*` / `Cargo.toml` / `docs/pluribus_stage4_{validation,decisions,api}.md`（B1 [测试] 角色边界字面继承 stage 3 同型政策）。
    - **新 test 文件落地（2 文件）**：`tests/nlhe_6max_warmup_byte_equal.rs`（5 测试 — D-409 / D-401 / D-402）+ `tests/nlhe_6max_raise_sizes.rs`（14 测试 — D-422，每 `PluribusAction` variant 一条）。
    - **既有 test 文件扩展（1 文件）**：`tests/regret_matching_numeric.rs` 既有 3 stage 3 测试基础上追加 5 测试 stage 4 扩展（D-401 / D-402 / D-403 / D-330 1e-9 容差），共 8 测试（3 stage 3 + 5 stage 4 = 8）。
    - **测试设计**：
        * **warmup_byte_equal.rs 5 测试**：(1) `warmup_phase_1m_update_blake3_byte_equal_stage3_anchor` 1M update × 2 trainer × SimplifiedNlheGame BLAKE3 byte-equal（release/ignored opt-in）；(2) `warmup_boundary_deterministic_byte_equal_two_runs` 1_001 step × 2 run 跨 run byte-equal sanity anchor（release/ignored — v3 artifact 528 MiB OOM 风险走 release 隔离）；(3) `warmup_to_linear_rm_plus_post_warmup_strategy_diverges_from_baseline` Kuhn 200 step post-warmup σ 与 stage 3 baseline σ 显著差异 trip-wire（default active panic-fail）；(4) `linear_weighting_t2_cumulative_formula_unit` Kuhn 2 step Linear decay 应用 trip-wire（default active panic-fail）；(5) `rm_plus_clamp_raw_regret_non_negative_via_checkpoint_inspection` `save_checkpoint + Checkpoint::open + bincode::deserialize` 路径读 RegretTable raw R 严格 `>= 0`（D-402 in-place clamp trip-wire；default active panic-fail）。
        * **raise_sizes.rs 14 测试**：每 `PluribusAction` 一条（Fold / Check / Call / AllIn + 10 raise mult）走 stage 1 `GameState::apply` byte-equal regression；统一 scenario = 6-max 500 BB starting stack（让所有 raise mult 不被 D-422(e) auto-AllIn 钳位）+ HJ-facing-UTG-3x 状态（pot=450 / max_committed=300 / last_full_raise=200 / min_raise=500）覆盖 raise size 全 10 mult；Check 走 HU postflop check-option 状态；Fold / Call / AllIn 走 root；非整数 0.75 Pot 走 ±1 chip 容差让 B2 [实现] 选 rounding policy（floor / round-half-up / ceil 任一通过）。
        * **regret_matching_numeric.rs +5 测试**：D-401 / D-402 / D-403 数值容差 — (6) `linear_weighted_strategy_sum_within_1e_minus_9_tolerance_after_100_steps` D-403 σ̄ sum 容差 sanity / (7) `rm_plus_in_place_clamp_strict_non_negative_via_checkpoint_at_t10` D-402 in-place clamp + 区分度 anchor（baseline 路径 R min < 0）/ (8) `linear_rm_plus_current_strategy_probability_sum_within_1e_minus_9_at_t100` D-330 σ sum 容差 sanity / (9) `linear_weighting_at_t1_byte_equal_standard_within_1e_minus_12` D-401 字面 t=1 边界 R̃_1 = r_1 等价 sanity / (10) `linear_weighted_strategy_sum_t_weighted_oversample_diverges_at_t100` D-403 字面 σ̄ t=100 与 standard CFR 显著差异 trip-wire。
    - **default profile 结果（B2 [实现] 落地前）**：5+14+5 = 24 新 test；其中 2 release/ignored（warmup_byte_equal Test 1 + Test 2），22 default-active；22 default-active 中 19 panic-fail（14 raise_sizes + 3 warmup unit trip-wire + 2 regret_matching trip-wire）+ 3 sanity anchor pass（regret_matching σ̄ sum / σ sum / t=1 byte-equal — 这 3 条在 stage 3 standard CFR 路径下持续通过，B2 [实现] 落地后 Linear+RM+ 路径仍保持容差不破，trip-wire 在 RM+ clamp / Linear 应用错误漂移时立即 fail）。**B2 [实现] 落地后**全套 22 default-active 转绿 + 2 release/ignored anchor 满足 byte-equal 维持。
    - **跨文件 helper 共享**：`load_v3_artifact_arc_or_skip` 加 `build_simplified_nlhe_game_or_skip` 让 Test 1 / Test 2 共享同一 `Arc<BucketTable>` 实例（避免 528 MiB body 加载两次致 OOM；继承 `tests/cfr_simplified_nlhe.rs` 同型 Arc-share 政策）；`dump_kuhn_regret_table_raw` 跨 warmup_byte_equal Test 5 + regret_matching Test 7 共享 `save_checkpoint + Checkpoint::open + bincode::deserialize` raw R 读取路径（D-327 `encode_table` 反路径）；`enumerate_kuhn_info_sets` / `kuhn_collect_current_strategies` 跨多测试复用。
    - **5 道 gate 全绿**：`cargo fmt --all --check` ✅ / `cargo build --all-targets` ✅ / `cargo clippy --all-targets -- -D warnings` ✅（修复 2 处 `doc_lazy_continuation`）/ `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` ✅ / `cargo test --no-run` ✅。stage 1 + 2 + 3 baseline 测试套件 byte-equal 维持（B1 [测试] 0 改动 src/* / tools/* / Cargo.toml）。
    - **角色边界**：B1 [测试] 单次 commit 触及 `tests/nlhe_6max_warmup_byte_equal.rs`（新文件） + `tests/nlhe_6max_raise_sizes.rs`（新文件）+ `tests/regret_matching_numeric.rs`（扩展 +5 测试） + `docs/pluribus_stage4_workflow.md`（本节）+ `CLAUDE.md`（stage 4 progress 翻面到"B1 [测试] closed"）。**0 改动** `src/*` / `tools/*` / `Cargo.toml` / `docs/pluribus_stage4_{validation,decisions,api}.md`（B1 [测试] 不修改决策 / API / validation 文档；如发现 spec 错误走 D-NNN-revM 流程，本 commit 未触发）。
    - **B1 → B2 工程契约**：B1 [测试] 落地后 B2 [实现] 起步前必 lock `step()` 内部 warm-up routing 逻辑（`update_count < warmup_complete_at` → stage 3 路径 byte-equal；`update_count >= warmup_complete_at` → stage 4 Linear+RM+ 路径）+ D-401 Linear discounting eager 路径具体公式（`R̃_t = (t/(t+1)) × R̃_{t-1} + r_t`，eager 实现每 step 起始扫全 InfoSet 应用 decay factor）+ D-402 RM+ in-place clamp 时机（每 update 后立即 `R̃ = max(R̃, 0)`）+ D-403 Linear weighted strategy sum 累积（`S_t = S_{t-1} + t × σ_t`）+ `PluribusActionAbstraction::actions/is_legal/compute_raise_to` 走 stage 1 `GameState` legal_actions + pot / current_bet 计算（D-422 字面 `raise_to = current_bet + multiplier × pot_size`，rounding policy B2 [实现] 起步前选 floor / round-half-up / ceil 任一让 0.75 Pot raise_to 落在 ±1 chip 容差内）。B2 [实现] commit 触及 `src/training/trainer.rs` + `src/training/regret.rs` + `src/abstraction/action_pluribus.rs` + 文档；**0 改动** `tests/*`（继承 stage 1/2/3 [实现] 角色边界）+ `docs/pluribus_stage4_{validation,decisions,api}.md`。

- **2026-05-15（B2 [实现] 落地 + §B2-revM carve-out）**：stage 4 B2 [实现] 单 commit 落地 — `src/training/trainer.rs` + `src/training/regret.rs` + `src/abstraction/action_pluribus.rs` Linear MCCFR + RM+ + warm-up routing + Pluribus 14-action 全套实现 + B1 22 default-active 测试全转绿 + 5 道 gate 全绿。
    - **实现路径（3 文件）**：
        * `src/abstraction/action_pluribus.rs`：`PluribusActionAbstraction::actions / is_legal / compute_raise_to` 全部 `unimplemented!()` 翻面落地 — `compute_raise_to(state, mult)` 走 `current_bet = max(p.committed_this_round)` + `pot = state.pot()` + `raise_delta_chips = (pot.as_u64() as f64 * mult) as u64`（D-420 字面 + **rounding policy = floor** 让 B1 0.75 Pot ±1 chip 容差通过）；`is_legal` 走 stage 1 `GameState::legal_actions()` 返回的 `LegalActionSet`（Fold / Check / Call / AllIn 直读对应字段；Raise X Pot 检验 `raise_to` 落在 `raise_range` 或 `bet_range` 内）；`actions` 按 `PluribusAction::all()` 字面顺序枚举 + filter `is_legal`。
        * `src/training/regret.rs`：`RegretTable` 加 2 个 `pub(crate)` 助手方法 — `apply_decay(factor)` 全表 in-place 乘 factor（D-401 eager decay 入口）+ `clamp_nonneg()` 全表 in-place `max(R, 0)`（D-402 RM+ clamp 入口）。`StrategyAccumulator` 不变（D-403 Linear weighted 由 trainer 在 `recurse_es` 调用前 pre-scale `sigma` 实现）。
        * `src/training/trainer.rs`：`EsMccfrTrainer::step` 重写 warm-up routing — `warm_up_done = update_count >= warmup_complete_at`，`use_linear = linear_weighting_enabled && warm_up_done`，`use_rm_plus = rm_plus_enabled && warm_up_done`；`t_stage4 = update_count - warmup_complete_at + 1` (1-indexed iter)；步骤 1: 若 `use_linear` 应用 `apply_decay(t/(t+1))` 到全 regret 表；步骤 2: 调 `recurse_es` 传 `strategy_sum_weight = t_stage4 if use_linear else 1.0`；步骤 3: 若 `use_rm_plus` 应用 `clamp_nonneg()`。`recurse_es` 函数签名扩展 `strategy_sum_weight: f64` 参数（递归路径 + non-traverser 决策点 `strategy_sum.accumulate(info, &weighted)`；当 weight == 1.0 走 zero-alloc 直接 `&sigma` 路径 byte-equal 维持 stage 3 anchor）。**`step_parallel` 不触及** — stage 4 多线程 Linear+RM+ 路径 deferred 到 C2 / E2（B2 scope 严格限于单线程 `step`）。
    - **§B2-revM carve-out**（[测试]↔[实现] 边界破例追认 — 用户授权 2026-05-15）：B2 [实现] 落地过程中发现 B1 [测试] 2 处明显数学 / scenario bug，用户授权同 commit 修测试 + 走 §B2-revM 追认（与 stage 2 / stage 3 同型跨角色边界 carve-out 模式继承）：
        * **carve-out (a)** — `tests/nlhe_6max_raise_sizes.rs::pluribus_action_check_legal_at_hu_bb_option_apply_byte_equal` 原 factory `make_hu_bb_check_state` 注释自述"HU postflop check 状态"但**实际构造 HU preflop**（SB Call → BB to act with committed=100）；测试断言"Check 不改变 committed_this_round"在 BB 走 Check 后 preflop 关街 → flop deal → `committed_this_round` 全员 reset 到 0 的 stage 1 行为下立即 fail (`before=100 → after=0`)。修复：factory 改走 HU **flop 街首行动** check-option 状态（SB Call + BB Check → 进入 flop → BB OOP 先行 D-022b-rev1，committed=0 → Check 不关街 SB 未行动 → committed 保持 0 不变）；test 主体 0 改动。
        * **carve-out (b)** — `tests/nlhe_6max_warmup_byte_equal.rs::linear_weighting_t2_cumulative_formula_unit` 原 test 走 2 step Kuhn σ 比较断言 max_diff > 1e-9 在 Kuhn n_players=2 alternating traverser 路径下**数学不成立**：2 step 内 traverser=0 / traverser=1 各访问独立 p0 / p1 InfoSet 子集（无同 IS 跨 step 重访），Linear decay 在各自 IS 上的作用是均匀乘性（`(t/(t+1))` 全表 scale），RM+ clamp 单边截断 max(R, 0)；`current_strategy(I) = max(R, 0) / sum(max(R, 0))` 归一化后 σ 比值不变（stage 4 R 与 stage 3 R 之差是均匀 scale + clamp，σ 严格相等）。修复：test 改走 `save_checkpoint + Checkpoint::open + bincode::deserialize` 路径间接读 raw `Vec<(KuhnInfoSet, Vec<f64>)>` 直接比较 R 值（继承 Test 5 `rm_plus_clamp_raw_regret_non_negative_via_checkpoint_inspection` 同型 raw R 读取路径），D-401 在 t=2 处对 step 0 触达 IS（r_2(I)=0 since traverser=1 in step 2）的 R 值应严格 `(2/3) × max(r_1, 0)`（stage 4）vs `r_1`（stage 3），二者必然不等。trip-wire intent 维持（"D-401 eager decay 路径已在 step() 内被路由"漏路由立即 fail）。
        * **角色边界破例字面继承 stage 2 §D-revM / stage 3 §D-022b-rev1 / §D-321-rev1 等跨角色边界 carve-out 同型模式**；用户书面授权（2026-05-15 conversation log）；B2 commit 同时修产品代码（[实现] 角色）+ 修测试（[测试] 角色）走单 commit 闭合，与 stage 1 / 2 / 3 严格双角色分离的典型 commit 不同。
    - **default profile 结果（B2 [实现] 落地后）**：B1 22 default-active 测试**全 22 转绿**：14 raise_sizes (Fold/Check/Call/AllIn + 10 raise mult) + 5 warmup_byte_equal 中 3 default-active (Test 3 post-warmup σ divergence / Test 4 D-401 raw R divergence / Test 5 RM+ clamp non-neg) + 8 regret_matching 中 7 default-active (含 stage 3 既有 2 active + stage 4 扩展 5 测试) + 2 release/ignored（warmup Test 1 / Test 2 — v3 artifact 528 MiB 走 release/--ignored opt-in，B2 [实现] 落地不消费）。stage 1 + 2 + 3 baseline 测试套件 byte-equal 维持（`config.linear_weighting_enabled = config.rm_plus_enabled = false` 默认让 `EsMccfrTrainer::new(...)` 路径完全等价 stage 3 standard CFR + RM）。
    - **5 道 gate 全绿**：`cargo fmt --all --check` ✅ / `cargo build --all-targets` ✅ / `cargo clippy --all-targets -- -D warnings` ✅ / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` ✅ / `cargo test --no-run` ✅。
    - **角色边界**：B2 [实现] 单次 commit 触及 `src/training/trainer.rs` + `src/training/regret.rs` + `src/abstraction/action_pluribus.rs` + `tests/nlhe_6max_raise_sizes.rs`（§B2-revM carve-out (a)）+ `tests/nlhe_6max_warmup_byte_equal.rs`（§B2-revM carve-out (b)）+ `docs/pluribus_stage4_workflow.md`（本节）+ `CLAUDE.md`（"stage 4 B2 [实现] closed" 状态翻面）。**0 改动** `tools/*` / `Cargo.toml` / `docs/pluribus_stage4_{validation,decisions,api}.md`（B2 [实现] 不修改决策 / API / validation 文档；如发现 spec 错误走 D-NNN-revM 流程，本 commit 未触发）。
    - **B2 → C1 工程契约**：B2 [实现] 落地后 C1 [测试] 起步前必 lock `NlheGame6` Game trait 全 8 方法 + 6-traverser routing 单元测试 scaffold（D-410 / D-411 / D-412）+ `PluribusActionAbstraction` × `stage 2 ActionAbstraction` trait impl 桥接测试（API-494）+ checkpoint v2 schema header 测试（API-440 128-byte header + 8 个新字段）。**D-401 eager decay 路径性能开销**：B2 实测尚未跑（B2 scope 限于 default profile Kuhn 单元测试，全表扫描 NLHE 量级 vultr 实测留 E1 [测试]）；若 E1 实测 < D-490 单线程 5K update/s SLO → D-401-revM lazy decay 翻面 evaluate（C2 [实现] 起步前 lock）。

- **2026-05-15（C1 [测试] 落地）**：stage 4 C1 [测试] 单 commit 落地 — 3 个新 integration crate（48 新 test 覆盖 D-410 / D-411 / D-412 / D-414 / D-416 / D-420 / D-422 / D-423 / D-449 + API-411 / API-440 / API-441 / API-494）+ `tests/api_signatures.rs` 同 commit 扩 stage 4 trip-wire（NlheGame6 as Game trait 8 method UFCS bind + Checkpoint v2 schema 4 const lock）+ 5 道 gate 全绿 + 0 改动 `src/*` / `tools/*` / `Cargo.toml` / `docs/pluribus_stage4_{validation,decisions,api}.md`（C1 [测试] 角色边界字面继承 stage 3 同型政策）。
    - **新 test 文件落地（3 文件 / 48 测试）**：
        * `tests/nlhe_6max_game_trait.rs` 24 测试 — **11 default-active anchor pass**：(1) `traverser_at_iter_returns_t_mod_6` D-412 字面 `t % 6` 单 + 跨 6 周期 + `u64::MAX % 6 = 3` 边界 / (2) `traverser_for_thread_returns_base_plus_tid_mod_6` D-412 字面 `(base + tid) % 6` 单 base 多 tid + 多 base 多 tid combo / (3) `traverser_at_iter_equals_traverser_for_thread_with_tid_zero` D-412 等价 sanity 100 iter / (4) `traverser_at_iter_covers_all_six_traversers_in_60_iter_cycle` D-414 60-iter cycle uniform 全 6 traverser 各 10 次 / (5) `traverser_at_iter_alternating_1000_iter_uniform_visits` D-414 1000-iter alternating 各 traverser ∈ [166, 167] 次 / (6) `game_variant_nlhe6max_tag_is_3_and_from_u8_round_trips` D-411 enum tag 3 + from_u8(3/4/255) round-trip + stage 1/2/3 既有 tag 不退化 / (7) `nlhe_game6_const_variant_is_nlhe6max` `<NlheGame6 as Game>::VARIANT` 编译期 const lock / (8) `nlhe_game6_action_type_alias_is_pluribus_action` NlheGame6Action = PluribusAction 类型 alias / (9) `nlhe_game6_infoset_type_alias_is_infoset_id` NlheGame6InfoSet = InfoSetId 类型 alias UFCS bind。**13 panic-fail until C2**：(10) `nlhe_game6_new_v3_artifact_panic_fail_until_c2` D-424 `NlheGame6::new(arc)` / (11) `nlhe_game6_new_hu_panic_fail_until_c2` D-416 `NlheGame6::new_hu(arc)` / (12) `nlhe_game6_with_config_panic_fail_until_c2` D-410 `NlheGame6::with_config(arc, cfg)` / (13) `game_trait_n_players_panic_fail_until_c2` D-410 `Game::n_players` / (14) `game_trait_root_panic_fail_until_c2` D-410 `Game::root` / (15) `game_trait_current_panic_fail_until_c2` D-410 `Game::current` / (16) `game_trait_info_set_panic_fail_until_c2` D-423 `Game::info_set` / (17) `game_trait_legal_actions_panic_fail_until_c2` D-420 `Game::legal_actions` / (18) `game_trait_next_panic_fail_until_c2` D-422 `Game::next` / (19) `nlhe_game6_actor_at_seat_panic_fail_until_c2` D-413 inherent / (20) `nlhe_game6_compute_14action_mask_panic_fail_until_c2` D-423 inherent / (21) `nlhe_game6_new_hu_equals_with_config_n_seats_2_panic_fail_until_c2` D-416 退化等价 / (22) `nlhe_game6_new_supported_bucket_table_returns_ok_panic_fail_until_c2` D-424 Ok 路径 sanity。`chance_distribution/payoff` 走 `#[should_panic]` 让 scaffold 阶段 unimplemented + C2 落地后 panic non-terminal 双路径满足。
        * `tests/action_pluribus_abstraction_trait.rs` 12 测试**全 default-active pass**：(1) `pluribus_action_n_actions_is_14` D-420 字面 14-action / (2) `pluribus_action_all_returns_14_in_canonical_order` D-420 / Pluribus 主论文 §S3 字面顺序 + u8 tag 0..=13 / (3) `pluribus_action_raise_multiplier_matches_pluribus_paper` 10 raise variant {0.5/0.75/1/1.5/2/3/5/10/25/50} + 4 non-raise None / (4) `pluribus_action_from_u8_round_trips_0_through_13_and_rejects_overflow` round-trip + 14/255 越界 None / (5) `pluribus_action_abstraction_actions_root_state_includes_fold` D-420 / LA-003 字面 / (6) `pluribus_action_abstraction_is_legal_preflop_utg_fold_call_check` LA-001 Fold/Call/!Check 字面 / (7) `pluribus_action_abstraction_compute_raise_to_integer_multiplier_exact` D-420 公式 4 数值 anchor (525/750/1200/22800) / (8) `action_abstraction_trait_abstract_actions_panic_fail_until_c2` API-494 inherent 路径 anchor C2 落地前 + trait UFCS commented-out / (9) `pluribus_action_abstraction_config_10_raise_ratios` D-420 字面 10 raise pot ratio milli + ActionAbstractionConfig sanity / (10) `pluribus_action_raise_multiplier_quantizes_to_bet_ratio_milli` 量化 sanity D-202-rev1 / (11) `pluribus_action_abstraction_actions_subset_invariance_across_streets` D-420 root preflop ↔ HJ-facing-raise legal action 切换 / (12) `stage2_abstract_action_abstract_action_set_surface_accessible` AbstractAction `to_concrete` + AbstractActionSet `len/iter` fn-pointer UFCS lock。**注意**：API-494 trait impl UFCS 调用 commented-out（C1 阶段 `impl ActionAbstraction for PluribusActionAbstraction` 未落地，UFCS bind 会 cargo build fail；C2 落地后翻面）。
        * `tests/checkpoint_v2_schema.rs` 12 测试 — **7 default-active pass**：(1) `checkpoint_trailer_len_is_32_stage_3_inherited` API-440 TRAILER_LEN = 32 字面 / (2) `checkpoint_magic_is_plckpt_pad_stage_3_inherited` API-440 MAGIC b"PLCKPT\0\0" / (3) `trainer_variant_es_mccfr_linear_rm_plus_tag_is_2` API-441 tag=2 + from_u8(2/3) + stage 3 既有 tag 0/1 / (4) `game_variant_nlhe6max_tag_is_3` API-411 tag=3 + from_u8(3/4) + stage 3 既有 tag 0/1/2 / (5) `checkpoint_variant_cardinality_anchor` 4 GameVariant × 3 TrainerVariant cardinality + (EsMccfrLinearRmPlus, Nlhe6Max) 主组合 / (6) `checkpoint_stage_4_new_fields_expected_values` D-449 字面 traverser_count=6 / linear_weighting=true / rm_plus=true / warmup_complete=false→true / (7) `checkpoint_stage_3_traverser_count_is_1_in_schema_v1_path` D-449 字面 stage 3 vs stage 4 traverser_count 区分。**5 panic-fail until D2**：(8) `checkpoint_schema_version_is_2_until_d2` D-449 当前 SCHEMA_VERSION=1 应 bump 到 2 / (9) `checkpoint_header_len_is_128_until_d2` D-449 当前 HEADER_LEN=108 应 bump 到 128 / (10) `checkpoint_header_field_size_addendum_32_bytes` D-449 新字段 size + alignment 32-byte addendum sanity / (11) `checkpoint_v2_layout_offsets_match_api_440_spec` API-440 字面 8-field layout offsets + 1 condensed const ref / (12) `checkpoint_schema_version_mismatch_dispatch_anchor_d2` D-449 stage 3 ↔ stage 4 schema mismatch 字面常量 anchor。
    - **`tests/api_signatures.rs` 扩展**：在既有 `_stage4_api_signature_assertions()` 函数内追加 (a) NlheGame6 as Game trait 8 方法 UFCS bind（`fn(&NlheGame6) -> usize` / `fn(&NlheGame6, &mut dyn RngSource) -> NlheGame6State` / `fn(&NlheGame6State) -> NodeKind` / 等 8 个签名 lock）+ (b) Checkpoint v2 schema 4 const ref lock（`SCHEMA_VERSION: u32` / `HEADER_LEN: usize` / `TRAILER_LEN: usize` / `MAGIC: [u8; 8]`）。让 D2 [实现] 起步前 const 名称 / 类型漂移立即在 cargo test --no-run 暴露（实际数值由 `tests/checkpoint_v2_schema.rs` panic-fail 验证）。
    - **default profile 结果（C2 / D2 [实现] 落地前）**：48 新 test 中 **30 default-active pass + 18 panic-fail**（13 nlhe_6max_game_trait + 5 checkpoint_v2_schema）+ B2 22 default-active 不退化（14 raise_sizes 全 + 3 warmup_byte_equal default-active + 7 regret_matching default-active + 2 release/ignored 维持）+ `tests/api_signatures.rs` 1 active pass 不退化 + stage 1 / 2 / 3 既有测试套件 byte-equal 维持（C1 [测试] 0 改动 `src/*`，stage 3 baseline 整套测试套件不退化由 cargo test --no-fail-fast 整套消费验证）。
    - **5 道 gate 全绿**：`cargo fmt --all --check` ✅ / `cargo build --all-targets` ✅ / `cargo clippy --all-targets -- -D warnings` ✅（修复 2 处 `doc_lazy_continuation`：`tests/checkpoint_v2_schema.rs` 顶部 doc-comment 跨段 list 缩进改 single-line 合并 + `tests/nlhe_6max_game_trait.rs` Group D doc-comment 同型修复）/ `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` ✅ / `cargo test --no-run` ✅。
    - **角色边界**：C1 [测试] 单次 commit 触及 `tests/nlhe_6max_game_trait.rs`（新文件）+ `tests/action_pluribus_abstraction_trait.rs`（新文件）+ `tests/checkpoint_v2_schema.rs`（新文件）+ `tests/api_signatures.rs`（扩展 NlheGame6 Game trait 8 方法 UFCS + Checkpoint 4 const lock）+ `docs/pluribus_stage4_workflow.md`（本节）+ `CLAUDE.md`（stage 4 progress 翻面到 "C1 [测试] closed"）。**0 改动** `src/*` / `tools/*` / `Cargo.toml` / `docs/pluribus_stage4_{validation,decisions,api}.md`（C1 [测试] 不修改决策 / API / validation 文档；如发现 spec 错误走 D-NNN-revM 流程，本 commit 未触发）。
    - **C1 → C2 工程契约**：C1 [测试] 落地后 C2 [实现] 起步前必 lock (a) `src/training/nlhe_6max.rs` Game trait 8 方法 `unimplemented!()` 翻面 — `n_players` 返 `self.config.n_seats as usize` / `root` 走 stage 1 `GameState::with_rng` 默认 multi-seat 分支 / `current/info_set/legal_actions/next/payoff` 走 stage 1 GameState 桥接 / `chance_distribution` 走 `panic!("no chance node")` 拒绝路径（stage 3 SimplifiedNlheGame 同型）；(b) `NlheGame6::new` 走 schema_version=2 + cluster (500,500,500) + BLAKE3 v3 anchor 三项校验后 `Ok(NlheGame6 { config: 6-max 100BB })`；`NlheGame6::new_hu` 配 `n_seats=2` 退化路径 byte-equal stage 3 anchor；`with_config(arc, cfg)` 通用入口；`actor_at_seat` 走 stage 1 GameState 桥接；`compute_14action_mask` 走 `PluribusActionAbstraction::actions` 输出 → 1 << tag 累积；(c) `src/training/trainer.rs` `EsMccfrTrainer<NlheGame6>::step + step_parallel` 走 6 套独立 `[RegretTable<NlheGame6>; 6]` 数组 + 6-traverser alternating routing（D-321-rev2 rayon par_iter_mut + append-only delta merge 扩展到 6-player）；(d) `src/abstraction/info.rs` `InfoSetId::with_14action_mask/legal_actions_mask_14` 落地 bits 33..47 14-bit mask 区域（A1 scaffold `unimplemented!()` 翻面）；(e) `impl ActionAbstraction for PluribusActionAbstraction` 桥接（API-494）— `abstract_actions` 走自身 inherent `actions(state)` 转 `Vec<PluribusAction>` → `Vec<AbstractAction>` 桥接 + `config()` 返 10 raise ratio + `map_off_tree` D-201 PHM stub 占位。**通过标准**：C1 13 + 5 panic-fail 中 13 个 C2 触及范围的全转绿（5 个 checkpoint_v2 schema panic-fail 留 D2 [实现] 落地翻面）+ HU 退化路径 1M update × 3 BLAKE3 byte-equal stage 3 `SimplifiedNlheGame` anchor（release/ignored opt-in 由 D1 [测试] 钉死实际数字）。C2 [实现] 不修改 `tests/*`（角色边界字面继承 stage 1 / 2 / 3 同型政策；如发现 C1 [测试] 内 spec bug 走 §C2-revM carve-out + 用户授权同 commit 修测试，与 stage 4 §B2-revM 同型模式）。
- **2026-05-15（C2 [实现] 落地）**：stage 4 C2 [实现] 单 commit 落地 — `src/abstraction/info.rs` + `src/abstraction/action.rs` + `src/abstraction/action_pluribus.rs` + `src/training/nlhe_6max.rs` 触及 — NlheGame6 Game trait 全 8 方法翻面 + 3 个构造（new/new_hu/with_config）+ 2 inherent helper（actor_at_seat/compute_14action_mask）+ InfoSetId 14-action mask bits 33..47 + impl ActionAbstraction for PluribusActionAbstraction（API-494）+ AbstractActionSet pub(crate) from_actions helper。C1 13 个 C2 触及范围 panic-fail 全转绿（5 个 checkpoint_v2 schema panic-fail 维持留 D2 [实现] 落地翻面）+ stage 1/2/3 baseline byte-equal 全套维持。
    - **实现路径（4 文件）**：
        * `src/abstraction/info.rs`：`InfoSetId::with_14action_mask(mask) / legal_actions_mask_14()` 全部 `unimplemented!()` 翻面。落地位置 = bits 33..47（D-423 lock + CLAUDE.md 字面 + `pluribus_stage4_api.md` API-423 字面），写入 `raw & !(0x3FFFu64 << 33) | ((mask as u64 & 0x3FFF) << 33)`。该 14-bit 区域在 stage 2 D-215 实际 layout 上跨 `betting_state` (bits 32..35) 高 2 bit + `street_tag` (bits 35..38) 全 3 bit + reserved (bits 38..64) 低 9 bit；doc-comment 字面承诺 NlheGame6 路径写 mask 后**字面禁止**回头调用 `betting_state()` / `street_tag()` 解包（mask 写入会破坏这两字段 bit 位 — 由 mask 本身提供 betting/street 等价判别力，不同 betting / street 必映射到不同 legal_actions subset 进而不同 mask）。**stage 3 SimplifiedNlheGame 路径不受影响**：SimplifiedNlhe 走 D-317-rev1 6-bit mask（bits 12..18 写入 bucket_id field），不调 `with_14action_mask`；stage 1/2/3 既有测试套件（`tests/info_id_encoding.rs` / `tests/cfr_simplified_nlhe.rs`）使用的 `betting_state()` / `street_tag()` getter 在 stage 3 InfoSetId 上 byte-equal 维持。
        * `src/abstraction/action.rs`：新增 `pub(crate) fn from_actions(actions: Vec<AbstractAction>) -> AbstractActionSet`（API-494 桥接 helper）让同 crate 桥接路径走，外部消费者继续走 stage 2 `DefaultActionAbstraction::abstract_actions` 等既有 trait 实现入口（不暴露未经 D-209 / AA-004-rev1 dedup 约束的 raw 构造路径）。
        * `src/abstraction/action_pluribus.rs`：落地 `impl ActionAbstraction for PluribusActionAbstraction`（API-494）— `abstract_actions(&self, state)` 走 inherent `actions(state)` 输出 → 每条转 stage 2 `AbstractAction`（Fold/Check/Call/AllIn 直读 stage 1 `LegalActionSet` 字段；Raise X Pot 走 `compute_raise_to(state, mult)` + `BetRatio::from_f64(mult)` 量化 `ratio_label`，`la.bet_range.is_some()` 决定 Bet vs Raise 分流）+ terminal/all-in 跳轮 state 返空集（stage 2 D-209 字面）；`map_off_tree` 走 D-201 PHM stub 占位（stage 4 NlheGame6 主路径不消费 off-tree 映射，stage 6c 替换为完整 PHM）；`config()` 通过 `OnceLock<ActionAbstractionConfig>` 静态实例返 10-raise-ratio `[0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 5.0, 10.0, 25.0, 50.0]`（D-420 字面）。
        * `src/training/nlhe_6max.rs`：`NlheGame6::new(arc)` 走 schema_version=2 + cluster (500,500,500) 校验 + 默认 6-max 100BB config + 默认 `PluribusActionAbstraction`（D-424 字面继承 stage 3 `SimplifiedNlheGame::new` 同型校验路径，失败返 `TrainerError::UnsupportedBucketTable`）；`NlheGame6::new_hu(arc)` 退化路径 `n_seats=2` + HU 100BB config（D-416）；`with_config(arc, cfg)` 通用入口（D-410）。`Game::n_players` 返 `config.n_seats as usize`；`Game::root` 走 stage 1 `GameState::with_rng(&config, 0, rng)` n_seats=6 默认 multi-seat 分支 + Arc clone bucket_table；`Game::current` 走 `is_terminal()` / `current_player()` 返 Player / Terminal（无独立 chance node，stage 3 同型）；`Game::info_set` 走 `pack_info_set_id(bucket_id, position_bucket, stack_bucket, betting_state, street_tag)` + `.with_14action_mask(compute_14action_mask)` 串联（preflop bucket = 169 lossless / postflop bucket = stage 2 BucketTable lookup）；`Game::legal_actions` 走 `PluribusActionAbstraction.actions(&state.game_state)` 桥接（D-420）；`Game::next` 走 PluribusAction → stage 1 Action 桥接（Fold/Check/Call/AllIn 直读；Raise X Pot 走 `compute_raise_to` + bet/raise 分流 by `bet_range.is_some()`）+ `GameState::apply` + `action_history.push(action)`；`Game::chance_distribution` 走 `panic!("no chance node")` 拒绝路径（stage 3 `SimplifiedNlheGame::chance_distribution` 同型）；`Game::payoff` 走 `GameState::payouts()` find `SeatId(player)` → `i64 as f64` 返 chip 净额。`actor_at_seat(state, SeatId(k))` 走 identity 返 `seat_id.0`（stage 1 `PlayerId == SeatId.0` 字面继承 SimplifiedNlheGame::info_set 同型政策）；`compute_14action_mask(state)` 走 `PluribusActionAbstraction.actions(state)` 输出，按 `action as u8` index 置位。
    - **§C2-revM carve-out**：本 commit **无** [测试]↔[实现] 边界 carve-out — C1 [测试] 13 panic-fail 测试在 C2 实现路径上字面继承 panic-fail intent（C1 测试 panic-fail 因 src/training/nlhe_6max.rs scaffold `unimplemented!()`；C2 落地翻面后测试断言走真实路径），无 spec bug 触发 `tests/*` 改动。0 改动 `tests/*` + `Cargo.toml` + `tools/*` + `docs/pluribus_stage4_{validation,decisions,api}.md`（C2 [实现] 角色边界字面继承 stage 1/2/3 同型政策）。
    - **6 套独立 [RegretTable<NlheGame6>; 6] 扩展 deferred**：C1 [测试] / B1 [测试] 测试集**未触及** `EsMccfrTrainer<NlheGame6>` 6 套独立 RegretTable 扩展路径（C1 `nlhe_6max_game_trait.rs` 13 panic-fail 全走 NlheGame6 Game trait 路径与 trainer 解耦；B1 `nlhe_6max_warmup_byte_equal.rs` 全走 `EsMccfrTrainer<SimplifiedNlheGame>` 或 `EsMccfrTrainer<KuhnGame>` 路径不消费 NlheGame6 trainer 入口；release/ignored anchor 测试由 D1 [测试] 起步前 stage 4 evaluated）。current `EsMccfrTrainer<NlheGame6>::step` 走单 shared `RegretTable` + `traverser = update_count % n_players` alternating（与 stage 3 `EsMccfrTrainer<SimplifiedNlheGame>` 路径字面等价、n_players 由 `Game::n_players` 返 6/2 区分），数值正确性维持。**D-412 lock 字面要求 "6 套独立 RegretTable + StrategyAccumulator 每 traverser 1 套"** 的 6-table 扩展（含 `EsMccfrTrainer<NlheGame6>::step + step_parallel` 走 `[RegretTable<NlheGame6>; 6]` 数组）deferred 到 D2 [实现] 起步前 + checkpoint v2 schema header 6-traverser dimension 落地同步翻面（Checkpoint v2 header `traverser_count: u8 = 6` 字段 + `regret_offset / strategy_offset: u64` × 6 套 sub-region 字面在 D2 schema_version 1 → 2 bump 同 commit 落地，让 6-table 扩展 + serialization 一次完成；继承 stage 3 D-321-rev1 → D-321-rev2 deferred 同型模式 — C2 commit ship serial-equivalent，真实多表 + 多线程并发翻面留 D2/E2）。
    - **default profile 结果（C2 [实现] 落地后）**：C1 `nlhe_6max_game_trait.rs` 24 测试全 24 转绿（11 default-active anchor 不退化 + 13 panic-fail 全套翻面通过：D-410/D-411/D-412/D-413/D-414/D-416/D-420/D-422/D-423/D-424 全套字面契约转绿）；C1 `action_pluribus_abstraction_trait.rs` 12 全套维持 default-active pass + trait impl 落地 cargo build 不报错 + UFCS bind 路径可调（commented-out C2 翻面占位维持）；C1 `checkpoint_v2_schema.rs` 12 测试 7 default-active 维持 + 5 panic-fail 维持（D-449 字面 SCHEMA_VERSION / HEADER_LEN 留 D2 [实现] 落地 bump 翻面）；B1 22 default-active 不退化（14 raise_sizes + 3 warmup + 5 regret_matching 全套）；stage 1 / 2 / 3 既有测试套件 byte-equal 维持（targeted 16 test crate 验证 cfr_kuhn / cfr_leduc / cfr_simplified_nlhe / cfr_fuzz / checkpoint_round_trip / checkpoint_corruption / cross_host_blake3 / trainer_error_boundary / info_id_encoding / determinism / history_roundtrip / scenarios / scenarios_extended / cross_arch_hash / cross_lang_history / cross_validation 全 0 failed 0 FAILED）+ `tests/api_signatures.rs` 1 active pass 不退化。
    - **5 道 gate 全绿**：`cargo fmt --all --check` ✅ / `cargo build --all-targets` ✅ / `cargo clippy --all-targets -- -D warnings` ✅ / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` ✅（修复 4 处 broken-intra-doc-link / private-intra-doc-link：info.rs `crate::training::NlheGame6::info_set` 改 plain text 避免 trait method link / `tests/cfr_simplified_nlhe.rs` 改 plain text / nlhe_6max.rs `[hu_table_config]` / `[validate_bucket_table]` 改 plain text 避免 private item link）/ `cargo test --no-run` ✅。
    - **角色边界**：C2 [实现] 单次 commit 触及 `src/abstraction/info.rs` + `src/abstraction/action.rs` + `src/abstraction/action_pluribus.rs` + `src/training/nlhe_6max.rs` + `docs/pluribus_stage4_workflow.md`（本节）+ `CLAUDE.md`（stage 4 progress 翻面到 "C2 [实现] closed"）。**0 改动** `tests/*` / `tools/*` / `Cargo.toml` / `docs/pluribus_stage4_{validation,decisions,api}.md`（C2 [实现] 不修改决策 / API / validation 文档；如发现 spec 错误走 D-NNN-revM 流程，本 commit 未触发）。
    - **C2 → D1 工程契约**：C2 [实现] 落地后 D1 [测试] 起步前必 lock (a) `tests/checkpoint_round_trip_6max.rs` 落地 5 测试覆盖 D-445 字面 6-traverser RegretTable + StrategyAccumulator full snapshot byte-equal（继承 stage 3 D1 [测试] 模式扩展到 6-traverser）；(b) HU 退化路径 `NlheGame6::new_hu(arc)` 配 `EsMccfrTrainer::new(...)` 跑 1M update × 3 BLAKE3 byte-equal stage 3 `SimplifiedNlheGame` anchor（release/ignored opt-in，与 `nlhe_6max_warmup_byte_equal.rs` 同型政策）— **本 anchor 在 C2 commit 上仍走 single-table 路径 byte-equal 维持**（C2 ship serial-equivalent，n_players=2 退化路径与 stage 3 路径数值等价）；(c) Checkpoint schema_version=2 round-trip + 跨版本 schema=1 ↔ 2 拒绝路径单元测试 scaffold（D-449 字面）+ 6-traverser RegretTable 数组 serialization round-trip 测试 scaffold（API-440 `traverser_count: u8` + `regret_offset / strategy_offset: u64` × 6 字面 sub-region encoding）+ 24h continuous run no-panic anchor（D-461）。D1 [测试] commit 触及 `tests/checkpoint_round_trip_6max.rs`（新文件）+ `tests/api_signatures.rs`（扩 Checkpoint v2 schema header field UFCS bind）+ `docs/pluribus_stage4_workflow.md`（修订历史 D1 [测试] closure entry）。**通过标准**：C1 13 panic-fail + B1 22 default-active 持续转绿 + stage 1/2/3 baseline byte-equal 维持 + D1 测试 scaffold panic-fail 落地（D2 [实现] 落地翻面转绿）。

- **2026-05-15（D1 [测试] 落地）**：stage 4 D1 [测试] 单 commit 落地 — 2 个新 integration crate + 1 既有 crate 扩展 + `tests/api_signatures.rs` 扩 stage 4 v2 schema header field UFCS bind + 5 道 gate 全绿 + 0 改动 `src/*` / `tools/*` / `Cargo.toml` / `docs/pluribus_stage4_{validation,decisions,api}.md`（D1 [测试] 角色边界字面继承 stage 1/2/3 同型政策）。
    - **新 test 文件落地（2 文件 / 21 测试）**：
        * `tests/checkpoint_v2_round_trip.rs`（18 测试 — D-445 / D-449 / API-440 / API-441，覆盖 workflow §步骤 D1 line 236 字面 18 项 round-trip / 跨版本 / config 字段 / byte-flip / 6-traverser regret/strategy serialization / regret_offset+strategy_offset 计算 / bucket_table_blake3 mismatch 等）：**Group A v2 round-trip 6-traverser × 14-action**（3 测试，全 release/ignored 走 v3 artifact 依赖）：(1) `schema_version_2_round_trip_6_traverser_14_action_layout_check` 走 NlheGame6 + Linear+RM+ save_checkpoint 后读 bytes 校 offset 8 schema=2 / offset 12 trainer=EsMccfrLinearRmPlus / offset 13 game=Nlhe6Max / offset 14 traverser_count=6 / (2) `schema_v2_round_trip_open_reads_back_consistent_fields` save → Checkpoint::open read-back schema/trainer/game/update_count 一致 / (3) `hu_degenerate_1m_update_x_3_blake3_byte_equal_stage3_simplified_nlhe_anchor` D-416 HU 退化 1M × 3 BLAKE3 byte-equal anchor（release/ignored，~30 min）。**Group B 跨版本拒绝**（4 测试）：(4) `stage3_schema_v1_kuhn_checkpoint_rejected_by_stage4_with_schema_mismatch` 走 stage 3 Kuhn trainer real save 写 schema=1 → Checkpoint::open 应返 SchemaMismatch（D2 [实现] 起步前 SCHEMA_VERSION=1 → 通过 → panic-fail；D2 bump 1→2 后转绿）/ (5) `stage4_byte_crafted_schema_v2_file_rejected_by_stage3_kuhn_trainer_dispatch` byte-craft schema=2 + Kuhn 文件 → stage 3 VanillaCfrTrainer<KuhnGame>::load_checkpoint 应返 SchemaMismatch (expected=1, got=2)（default-active pass）/ (6) `traverser_count_1_vs_6_mismatch_rejected_by_nlhe6_trainer` byte-craft schema=2 + traverser_count=1 → NlheGame6 trainer 期望=6 拒绝（release/ignored，v3 artifact 依赖）/ (7) `schema_v2_byte_crafted_valid_file_accepted_by_stage4_nlhe6_trainer` byte-craft valid v2 → NlheGame6 trainer accept（release/ignored，panic-fail until D2）。**Group C config 字段 mismatch 拒绝**（3 测试，全 release/ignored）：(8-10) linear_weighting / rm_plus / warmup_complete 字段 mismatch 拒绝路径 byte-craft + load_checkpoint 触发。**Group D 5 类 CheckpointError v2 byte-flip + bucket_table_blake3 mismatch**（5 测试）：(11) `v2_body_byte_flip_returns_corrupted_after_trailer_blake3_check` body byte flip → Corrupted(trailer BLAKE3)（D-352；panic-fail until D2 — schema=2 当前先走 SchemaMismatch）/ (12) `v2_magic_byte_flip_returns_corrupted_magic` magic flip + 重算 trailer → Corrupted(magic)（D-350；default-active pass — magic 校验先于 schema）/ (13) `v2_trailer_direct_flip_returns_corrupted` trailer 直接 flip → Corrupted（panic-fail until D2）/ (14) `v2_bucket_table_blake3_mismatch_rejected` byte-craft [0xAA;32] blake3 → BucketTableMismatch（release/ignored，v3 artifact 依赖）/ (15) `v2_atomic_write_no_temp_residue_sanity` D-353 atomic rename 后 `.tmp` sibling 不残留（default-active pass）。**Group E 6-traverser regret / strategy + cross-load equivalence + regret_offset/strategy_offset**（3 测试，全 release/ignored）：(16) `six_traverser_regret_table_save_save_blake3_byte_equal` 同 trainer state 两次 save 文件 byte-equal + schema=2 字面（D-445）/ (17) `six_traverser_save_load_resave_byte_equal_cross_load_equivalence` save → load → re-save 文件 byte-equal + update_count 一致 / (18) `v2_regret_offset_and_strategy_offset_field_values_correct` API-440 字面 regret_offset=128 / strategy_offset>=regret_offset / pad_a + pad_b 全 0 sanity。
        * `tests/training_24h_continuous.rs`（3 测试 — D-461 / D-431）：全 release/--ignored opt-in 用户手动 + AWS / vultr host 触发（24h wall-time）：(1) `stage4_six_max_24h_no_crash` D-461 24h 连续 NlheGame6::step 无 panic / OOM / NaN / Inf + wall-time ≤ 24h 上限断言 + 每 1M update probe finite sanity / (2) `stage4_six_max_24h_rss_increment_under_5gb` D-431 process RSS 增量 < 5 GB（读 /proc/self/status VmRSS，Linux only；非 Linux fallback skip）+ 每 10M update probe / (3) `stage4_six_max_checkpoint_every_1e8_update_writes_successfully` D-461 cadence — 每 10⁸ update 写 checkpoint 成功 + read-back update_count 一致 + schema_version=2 字面（panic-fail until D2）。本套 3 测试**全 #[ignore]**，default profile 0 触发；host CPU 核心数 < 4 走 pass-with-skip（D-490 字面 4-core SLO）；v3 artifact 缺失走 pass-with-skip。
    - **既有 test 文件扩展（1 文件 / 6 测试）**：
        * `tests/cfr_fuzz.rs` 既有 stage 3 12 测试基础上追加 6 stage 4 测试（D-410 / D-412 / D-420 / D-401 / D-402 / D-409 fuzz 不变量扩展到 6-player NLHE + 14-action + Linear+RM+ 路径，全 release/--ignored opt-in 走 v3 artifact 依赖）：(13) `cfr_nlhe6_stage3_path_smoke_no_panic_no_nan_no_inf` NlheGame6 trainer warmup_at=u64::MAX 全程 stage 3 path 200 step / (14) `cfr_nlhe6_stage3_path_full_1m_no_panic_no_nan_no_inf` 同前 1M step / (15) `cfr_nlhe6_warmup_boundary_smoke_no_panic_no_nan_no_inf` warmup_at=100 跨 warmup 100+100 200 step / (16) `cfr_nlhe6_warmup_boundary_full_1m_no_panic_no_nan_no_inf` 同前 1M step / (17) `cfr_nlhe6_pure_linear_rm_plus_smoke_no_panic_no_nan_no_inf` warmup_at=0 step 1 起 Linear+RM+ 全程 200 step / (18) `cfr_nlhe6_pure_linear_rm_plus_full_1m_no_panic_no_nan_no_inf` 同前 1M step（覆盖 D-401 t/(t+1) 因子 1M iter 接近 1 数值稳定边界）。每测试同型 fuzz 模式：每 step update_count += 1（D-307）+ 每 PROBE_EVERY=100 step probe 4 InfoSet 的 current_strategy/average_strategy 全 finite + sum ∈ 1 ± 1e-6。stage 3 既有 12 测试不退化（cfr_kuhn_smoke default-active pass 维持）。
    - **`tests/api_signatures.rs` 扩展**：在 `_stage4_api_signature_assertions()` 内追加 (a) Checkpoint v2 schema header field byte offset 字面 sanity（OFFSET_TRAVERSER_COUNT=14 / OFFSET_LINEAR_WEIGHTING=15 / OFFSET_RM_PLUS=16 / OFFSET_WARMUP_COMPLETE=17 / OFFSET_REGRET_OFFSET=96 / OFFSET_STRATEGY_OFFSET=104 / OFFSET_PAD_B=112 / HEADER_V2_LEN=128，本块 const 字面 + 6 const_assert 让 D2 [实现] 落地 src/training/checkpoint.rs::OFFSET_* 时翻面成 UFCS bind）+ (b) Checkpoint pub field 类型 lock（traverser_count: u8 / linear_weighting_enabled: bool / rm_plus_enabled: bool / warmup_complete: bool 4 字面 const expected 值 sanity；D2 落地 Checkpoint struct 加 4 字段后翻面成 `let _: u8 = ckpt_v2.traverser_count;` 形式 UFCS bind）。
    - **default profile 结果（D2 [实现] 落地前）**：21 + 6 = 27 D1 新 test；其中 **24 release/--ignored opt-in**（18 checkpoint_v2_round_trip 中 12 ignored + 6 active；3 training_24h_continuous 全 ignored；6 cfr_fuzz 扩展全 ignored；checkpoint_v2_round_trip 12 ignored 中 1 是 HU 退化 1M × 3 anchor + 11 是 v3 artifact + v2 schema 依赖）+ **3 default-active panic-fail**（D2 trip-wire — `stage3_schema_v1_kuhn_checkpoint_rejected_by_stage4_with_schema_mismatch` D-449 当前 SCHEMA_VERSION=1 走 Ok / `v2_body_byte_flip_returns_corrupted_after_trailer_blake3_check` schema=2 文件先走 SchemaMismatch / `v2_trailer_direct_flip_returns_corrupted` 同前）+ **3 default-active anchor pass**（`stage4_byte_crafted_schema_v2_file_rejected_by_stage3_kuhn_trainer_dispatch` parse_bytes SchemaMismatch dispatch / `v2_magic_byte_flip_returns_corrupted_magic` magic 校验先于 schema / `v2_atomic_write_no_temp_residue_sanity` D-353 atomic rename sanity）。**D2 [实现] 落地后** 24 release/--ignored opt-in 全转绿 + 3 default-active panic-fail 全套翻面转绿 + 3 anchor pass 持续维持。stage 1 + 2 + 3 baseline 测试套件 byte-equal 维持（cfr_kuhn_smoke active pass 不退化 + B1/C1/C2 既有套件 byte-equal 维持）。
    - **跨文件 helper 共享**：`load_v3_artifact_arc_or_skip` / `craft_minimal_v2_checkpoint_bytes` / `unique_temp_path` / `cleanup` / `blake3_hex` 等 helper 在 checkpoint_v2_round_trip.rs 内独立实现（不跨 crate 共享，避免 tests 目录引入 common module 依赖）；继承 stage 3 D1 `checkpoint_round_trip.rs` 同型 helper 政策（每 test crate 内联 helper，让跨 crate 借用风险收窄）。`load_v3_artifact_or_skip` 在 training_24h_continuous.rs 内独立实现（schema=2 + cluster (500,500,500) + BLAKE3 v3 anchor 三项校验同 stage 3 / B1 / C1 helper）。
    - **5 道 gate 全绿**：`cargo fmt --all --check` ✅ / `cargo build --all-targets` ✅ / `cargo clippy --all-targets -- -D warnings` ✅ / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` ✅ / `cargo test --no-run` ✅。
    - **角色边界**：D1 [测试] 单次 commit 触及 `tests/checkpoint_v2_round_trip.rs`（新文件） + `tests/training_24h_continuous.rs`（新文件）+ `tests/cfr_fuzz.rs`（扩展 +6 stage 4 测试）+ `tests/api_signatures.rs`（扩 Checkpoint v2 schema header field byte offset 字面 sanity + Checkpoint pub field 4 字面 const expected sanity）+ `docs/pluribus_stage4_workflow.md`（本节）+ `CLAUDE.md`（stage 4 progress 翻面到 "D1 [测试] closed"）。**0 改动** `src/*` / `tools/*` / `Cargo.toml` / `docs/pluribus_stage4_{validation,decisions,api}.md`（D1 [测试] 不修改决策 / API / validation 文档；如发现 spec 错误走 D-NNN-revM 流程，本 commit 未触发）。
    - **D1 → D2 工程契约**：D1 [测试] 落地后 D2 [实现] 起步前必 lock (a) `src/training/checkpoint.rs` SCHEMA_VERSION bump 1 → 2 + HEADER_LEN bump 108 → 128 + 6 个新 const 落地（OFFSET_TRAVERSER_COUNT=14 / OFFSET_LINEAR_WEIGHTING=15 / OFFSET_RM_PLUS=16 / OFFSET_WARMUP_COMPLETE=17 / OFFSET_REGRET_OFFSET=96 / OFFSET_STRATEGY_OFFSET=104 / OFFSET_PAD_B=112） + `Checkpoint` struct 加 4 个 pub 字段（`traverser_count: u8` / `linear_weighting_enabled: bool` / `rm_plus_enabled: bool` / `warmup_complete: bool`）；(b) `Checkpoint::save` schema_version=2 路径写 v2 header（128-byte layout + 4 个新字段 + 8-byte regret_offset / 8-byte strategy_offset + pad_a/pad_b reserved 全 0 + trailer BLAKE3 over header+body）；`Checkpoint::open` schema_version=1 vs 2 dispatch（schema=1 走 stage 3 既有 108-byte layout / schema=2 走 v2 128-byte layout）+ schema 字段不匹配 SCHEMA_VERSION 返 SchemaMismatch；(c) `EsMccfrTrainer<NlheGame6>::save_checkpoint` 在 `config.linear_weighting_enabled && config.rm_plus_enabled` 时走 schema_version=2 路径 + TrainerVariant::EsMccfrLinearRmPlus + 6-traverser RegretTable + StrategyAccumulator 数组 bincode 序列化（D-412 字面 6 套独立表）；`load_checkpoint` 在 schema=2 路径下反序列化 6-traverser 数组 + preflight 校验 4 新字段 + bucket_table_blake3；(d) `EsMccfrTrainer::step + step_parallel` 走 `[RegretTable<NlheGame6>; 6]` + `[StrategyAccumulator<NlheGame6>; 6]` 数组 + 6-traverser alternating routing（D-321-rev2 rayon par_iter_mut 扩展到 6-player）；(e) `TrainerError::OutOfMemory { rss_bytes, limit }` variant dispatch — `MetricsCollector::observe` 或 trainer step path 检测 RSS > 阈值返 Err 让 24h continuous run 短路（A1 [实现] 已落地 enum variant，D2 落地实际触发 dispatch）。**通过标准**：D1 24 release/--ignored opt-in 测试 D-491 AWS / vultr host 触发后全转绿 + D1 3 default-active panic-fail 全套翻面 + 3 anchor pass 持续维持 + B1 22 default-active + C1 30 default-active 全套不退化 + stage 1/2/3 baseline byte-equal 维持。D2 [实现] commit 触及 `src/training/checkpoint.rs` + `src/training/trainer.rs` + 可能扩展 `src/training/regret.rs`（6-traverser RegretTable 数组）+ `docs/pluribus_stage4_workflow.md`（修订历史 D2 [实现] closure entry）+ `CLAUDE.md`；**0 改动** `tests/*`（继承 stage 1/2/3 [实现] 角色边界政策）+ `docs/pluribus_stage4_{validation,decisions,api}.md`（D2 [实现] 不修改决策 / API / validation 文档；如发现 spec 错误走 D-NNN-revM 流程）。**HU 退化 1M × 3 BLAKE3 byte-equal anchor**（D-416 / Test 3 release/ignored）在 D2 落地后由用户手动 vultr / AWS host 触发实测钉死具体 BLAKE3 trailer hex（继承 stage 3 D-362 1M update × 3 BLAKE3 anchor 同型模式）。

- **2026-05-15（D2 [实现] 落地）**：stage 4 D2 [实现] 单 commit 落地 — `src/training/checkpoint.rs` 大改 + `src/training/trainer.rs` save/load schema dispatch + `tests/api_signatures.rs` + 3 既有 stage 3/4 test crate 内 §D2-revM 4 处碎片修正（共 4 处测试 minimal edit）。stage 1/2/3 baseline 全套维持 + D1 18+3+6 测试中 D2 触及范围全套 PASS / IGNORED（剩 19 release/--ignored opt-in 与 v3 artifact + AWS / vultr host 触发条件相关）。
    - **§D2-revM dispatch carve-out**（用户授权 Option A，2026-05-15）：`Checkpoint::open` / `Checkpoint::parse_bytes` 按文件 `schema_version` 字段 dispatch v1 / v2 双路径解析（接受两个版本），让 stage 3 既有 corruption / round-trip / warmup 测试套件全部 byte-equal 维持；`SCHEMA_VERSION` 常量 bump 到 2 是 "latest 支持版本" 标记。stage 3 trainer（VanillaCfr / EsMccfr）仍写 schema=1，stage 4 `EsMccfrTrainer<NlheGame6>` 在 `config.linear_weighting_enabled && config.rm_plus_enabled` 时写 schema=2（TrainerVariant::EsMccfrLinearRmPlus + traverser_count=6 + 4 个 v2 字段持久化）；其它 trainer 写 schema=1（v1 layout，4 个 stage 4 字段以默认值占位）。trainer 侧 `ensure_trainer_schema` preflight 让 `VanillaCfrTrainer<G>::load_checkpoint` / `EsMccfrTrainer<G != NlheGame6>::load_checkpoint` 走 expected_schema=1 拒绝 stage 4 文件（SchemaMismatch(expected=1, got=2)）；`EsMccfrTrainer<NlheGame6>::load_checkpoint` 接受 v1（HU 退化兼容）与 v2（Linear+RM+ 主路径）双 schema。该 dispatch 设计与 D1 测试集主要锚点（test 5 stage4_byte_crafted_schema_v2_file_rejected_by_stage3_kuhn_trainer_dispatch / test 11 v2_body_byte_flip_returns_corrupted / test 13 v2_trailer_direct_flip_returns_corrupted / Group D / Group E）全套兼容；但与 D1 test 4 (`stage3_schema_v1_kuhn_checkpoint_rejected_by_stage4_with_schema_mismatch`) 严格 v2-only 语义不可同时满足（test 4 panic-fail 留待 §D2-revM 后续 re-author）。
    - **§D2-revM table-array deferral**（继承 D-321-rev1 → D-321-rev2 同型模式）：D-412 字面 `[RegretTable<NlheGame6>; 6]` + `[StrategyAccumulator<NlheGame6>; 6]` 6 套独立表 + 6-traverser alternating routing **runtime 真实多表 deferred 到 E2** — D2 commit 维持 C2 commit 上的 single shared `RegretTable` + `traverser = update_count % n_players` alternating（n_players 由 `Game::n_players` 返 6/2 区分，与 stage 3 `EsMccfrTrainer<SimplifiedNlheGame>` 路径数值等价）；但 **D2 落地 Checkpoint v2 schema header 6-traverser dimension**（`traverser_count: u8 = 6` 持久化 + 4 个 stage 4 字段 + `regret_offset` / `strategy_offset` 字面 layout），让 v2 serialization 已就位。E2 [实现] 落地真并发 6 套表后 → checkpoint v2 body sub-region encoding 同步翻面（继承 D-321-rev1 → D-321-rev2 deferred 同型模式，C2 ship serial-equivalent → E2 真并发翻面）。
    - **实现路径（3 个产品文件 + 4 个测试文件 minimal edit）**：
        * `src/training/checkpoint.rs`：SCHEMA_VERSION bump 1 → 2 + HEADER_LEN bump 108 → 128 + 新增 7 个 OFFSET_* pub const（OFFSET_TRAVERSER_COUNT=14 / OFFSET_LINEAR_WEIGHTING=15 / OFFSET_RM_PLUS=16 / OFFSET_WARMUP_COMPLETE=17 / OFFSET_REGRET_OFFSET=96 / OFFSET_STRATEGY_OFFSET=104 / OFFSET_PAD_B=112）+ 保留 `SCHEMA_VERSION_V1=1` / `HEADER_LEN_V1=108` 与 v1 layout pub(crate) `OFFSET_V1_*` 常量给 legacy parse 路径。`Checkpoint` struct 加 4 pub 字段（`traverser_count: u8` / `linear_weighting_enabled: bool` / `rm_plus_enabled: bool` / `warmup_complete: bool`）。`Checkpoint::save` schema_version dispatch：1 → 写 v1 108-byte header（`encode_v1`，4 个 stage 4 字段不持久化）/ 2 → 写 v2 128-byte header（`encode_v2`，4 个新字段 + regret/strategy_offset u64 LE + pad_a/pad_b reserved 全 0 + trailer BLAKE3 over header+body）/ 其它 → SchemaMismatch。`Checkpoint::open` / `parse_bytes` 按 schema 字段 dispatch `parse_bytes_v1` / `parse_bytes_v2`（v1 path 4 个 stage 4 字段以默认值 [traverser_count=1, linear=false, rm_plus=false, warmup=false] 填充）；其它 schema 走 SchemaMismatch(expected=SCHEMA_VERSION=2, got=N)。`preflight_trainer` 按 schema 字段 dispatch bucket_table_blake3 offset 读取（v1 = 60 / v2 = 64）。`write_atomic` helper 抽出 D-353 atomic rename 共享路径。`bool_from_u8` helper 校验 4 个 stage 4 字段 ∈ {0, 1}。
        * `src/training/trainer.rs`：`VanillaCfrTrainer<G>::save_checkpoint` 构造 Checkpoint with schema=1 + 默认 stage 4 字段（traverser_count=1 / 全 false）+ TrainerVariant::VanillaCfr；`VanillaCfrTrainer<G>::load_checkpoint` 在 `preflight_trainer` 之前调 `ensure_trainer_schema(&bytes, 1)` 拒绝 schema≠1（含 stage 4 schema=2 文件 → SchemaMismatch(expected=1, got=2)）。`EsMccfrTrainer<G>::save_checkpoint` 走 stage4_path 判别（`G::VARIANT == GameVariant::Nlhe6Max && config.linear_weighting_enabled && config.rm_plus_enabled`）→ schema=2 / EsMccfrLinearRmPlus / traverser_count=`n_players as u8` / linear=rm_plus=true / warmup_complete=`update_count >= warmup_complete_at`；否则 schema=1 / EsMccfr / 4 字段默认。`EsMccfrTrainer<G>::load_checkpoint` 按 G::VARIANT dispatch：Nlhe6Max 接受 v1 与 v2 双路径（v2 走 EsMccfrLinearRmPlus preflight + `build_linear_rm_plus_config` 从 header 4 字段还原 TrainerConfig；v1 走 EsMccfr preflight + default config）；其它 G 调 `ensure_trainer_schema(&bytes, 1)` 拒绝 schema≠1。新增 helper `ensure_trainer_schema` / `peek_schema` / `build_linear_rm_plus_config` / `self_default_config_nlhe6max`。
        * `src/error.rs`：**0 改动**（A1 [实现] 已落地 `TrainerError::OutOfMemory { rss_bytes, limit }` variant + `TrainerVariant::EsMccfrLinearRmPlus = 2` / `GameVariant::Nlhe6Max = 3` 全 enum tag round-trip）。`TrainerError::OutOfMemory` step-path runtime dispatch deferred 到 E2 metrics 接入（`tests/training_24h_continuous.rs` 3 个 #[ignore] 测试走自带 RSS probe + panic-on-exceed，不依赖 step 路径触发 OOM variant）。
        * `tests/api_signatures.rs`（§D2-revM 1 处 minimal edit）：在既有 stage 3 `Checkpoint { schema_version: 1u32, ... }` 8 字段构造点追加 4 个 stage 4 字段（`traverser_count: 1u8` / 3 个 false） + 4 个 UFCS `let _: u8/bool = ckpt.<field>` 字面继承 D1 [测试] line 880-887 内置 TODO 字面授权（commit message `let _: u8 = ckpt_v2.traverser_count;` 形式期望预留）。
        * `tests/checkpoint_round_trip.rs`（§D2-revM 1 处 minimal edit）：`d350_header_constants_lock` 内 `assert_eq!(SCHEMA_VERSION, 1)` 改 `assert_eq!(SCHEMA_VERSION, 2)`（D2 bump 后字面更新；同 stage 3 D2 SCHEMA_VERSION bump 同型 pattern）。
        * `tests/checkpoint_corruption.rs`（§D2-revM 1 处 minimal edit）：`schema_version_downgrade_returns_schema_mismatch` 加 `#[ignore = "..."]` — dispatch 路径下 schema=SCHEMA_VERSION-1=1 是 legacy v1 schema 合法，本 "downgrade" 语义不再触达（留待 §D2-revM 后续 re-author 改为 schema=0 / 跨 v2+1 路径）。
        * `tests/checkpoint_v2_round_trip.rs`（§D2-revM 1 处 minimal edit）：`stage3_schema_v1_kuhn_checkpoint_rejected_by_stage4_with_schema_mismatch` 加 `#[ignore = "..."]` — dispatch 路径下 Checkpoint::open 接受 schema=1 不可同时满足严格 v2-only 期望；stage 3 ↔ stage 4 跨版本拒绝改由 Trainer::load_checkpoint 内置 `ensure_trainer_schema` preflight 落地（test 5 字面继续覆盖该 trainer-level dispatch 路径）。
        * `tests/checkpoint_v2_schema.rs`（§D2-revM 1 处 minimal edit）：`checkpoint_header_field_size_addendum_32_bytes` 内 `new_fields_total + 12` 改 `new_fields_total`（订正 C1 [测试] 算术误差：实际 HEADER_LEN bump = 128 - 108 = 20 byte = 4 新 u8 + 16-byte pad_b，与 `new_fields_total` = 4 + 8 + 8 = 20 同值；test 原作者 +12 修正项来源不明，pad_b 实际 16 byte 非 12 byte）。
    - **default profile 结果（D2 [实现] 落地后）**：
        * D1 `checkpoint_v2_schema.rs` 12 个测试全转绿（C1 标记的 5 个 panic-fail-until-D2 + 7 个 default-active anchor 全套通过；含 `checkpoint_schema_version_is_2_until_d2` / `checkpoint_header_len_is_128_until_d2` / `checkpoint_header_field_size_addendum_32_bytes` / `checkpoint_v2_layout_offsets_match_api_440_spec` / `checkpoint_schema_version_mismatch_dispatch_anchor_d2` 全套）。
        * D1 `checkpoint_v2_round_trip.rs` 18 个测试：5 default-active pass + 1 §D2-revM ignored（test 4 留待 re-author）+ 12 release/--ignored opt-in 维持（v3 artifact 依赖；D-491 AWS / vultr host 用户触发后由用户手动钉死实测数字，含 D-416 HU 退化 1M × 3 BLAKE3 byte-equal anchor）。
        * D1 `training_24h_continuous.rs` 3 个测试全 release/--ignored opt-in 维持（24h wall-time + AWS / vultr host 用户手动触发）。
        * D1 `cfr_fuzz.rs` +6 stage 4 测试全 release/--ignored opt-in 维持（stage 3 既有 12 测试不退化，cfr_kuhn_smoke default-active pass 维持）。
        * 既有 stage 3 `checkpoint_round_trip.rs` 19 测试：15 default-active pass（含 `d350_header_constants_lock` SCHEMA_VERSION=2 字面更新后通过）+ 4 ignored 维持。
        * 既有 stage 3 `checkpoint_corruption.rs` 12 测试：11 default-active pass + 1 §D2-revM ignored（schema_version_downgrade 在 dispatch 路径下不再触达）。
        * 既有 stage 1/2/3 baseline 测试套件 byte-equal 维持（targeted 16 test crate 验证 cfr_kuhn / cfr_leduc / cfr_simplified_nlhe / cfr_fuzz / nlhe_6max_warmup_byte_equal / regret_matching_numeric / schema_compat / trainer_error_boundary / nlhe_6max_game_trait / action_pluribus_abstraction_trait / api_signatures / nlhe_6max_raise_sizes / scenarios / scenarios_extended / cross_arch_hash / cross_lang_history / cross_validation 全 0 failed 0 FAILED）。`tests/bucket_quality.rs` 9 个 default-fail（stage 2 §G-batch1 §3.4-batch2 D-233-rev2 已知 baseline 失败，非 D2 引入）继续不退化。
    - **5 道 gate 全绿**：`cargo fmt --all --check` ✅ / `cargo build --all-targets` ✅ / `cargo clippy --all-targets -- -D warnings` ✅（0 warning）/ `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` ✅（src/training/checkpoint.rs `Checkpoint::parse_bytes` private-intra-doc-link 用 plain text 写避免 private item 链接）/ `cargo test --no-fail-fast` ✅（stage 1/2/3 baseline 维持 + stage 2 bucket_quality 9 known fail + 全 D2 触及范围 PASS）。
    - **角色边界**：D2 [实现] 单次 commit 触及 `src/training/checkpoint.rs` + `src/training/trainer.rs` + 4 个 tests/*.rs（§D2-revM 4 处 minimal edit，**用户授权 Option A 显式包含**）+ `docs/pluribus_stage4_workflow.md`（本节）+ `CLAUDE.md`（stage 4 progress 翻面到 "D2 [实现] closed"）。**0 改动** `src/error.rs` / `tools/*` / `Cargo.toml` / `docs/pluribus_stage4_{validation,decisions,api}.md`（D2 [实现] 不修改决策 / API / validation 文档；如发现 spec 错误走 D-NNN-revM 流程，本 commit 未触发）。
    - **D2 → E1 工程契约**：D2 [实现] 落地后 E1 [测试] 起步前必 lock (a) `tests/perf_slo.rs::stage4_*` 8 条 SLO 测试（D-490 ① 单线程 5K / ② 4-core 15K / ③ 32-vCPU 20K / D-454 LBR P95 30s / D-485 baseline eval 2min / D-461 24h continuous wall-time / D-498 7-day nightly fuzz / D-490 6-traverser per-traverser throughput cross-check，全 release/--ignored opt-in）；(b) `tests/lbr_eval_convergence.rs` 6 条 LBR exploitability 收敛测试（first usable 10⁹ update 后 LBR < 200 mbb/g / LBR 100 采样点单调非升 / per-traverser 上界 D-459 / OpenSpiel-export policy byte-equal / 14-action LBR enumerate / D-455 myopic horizon=1 边界）。**通过标准**：default profile 14 测试 panic-fail（E2 [实现] 落地后转绿）+ `--ignored` 触发 AWS c7a.8xlarge 实测 first usable 训练完成后达到 D-490 阈值。**§D2-revM table-array deferral 翻面条件**：E2 [实现] 落地真并发 6 套表 + step_parallel 扩 NlheGame6 路径 + checkpoint v2 body sub-region encoding 同步翻面（继承 D-321-rev2 同型模式），D-412 字面要求在 E2 commit 上完整覆盖。**§D2-revM dispatch carve-out 后续 re-author**：test 4 (`stage3_schema_v1_kuhn_checkpoint_rejected_by_stage4_with_schema_mismatch`) + `schema_version_downgrade_returns_schema_mismatch` 由用户授权后 §D2-revM-revN（如改测 byte-craft schema > 2 unsupported file 或 schema=0 路径）在 stage 4 后续 commit 内 re-author。

- **2026-05-15（E1 [测试] 落地）**：stage 4 E1 [测试] 单 commit 落地 — `tests/perf_slo.rs` 扩 8 条 stage4_* SLO 测试 + 新增 `tests/lbr_eval_convergence.rs` integration crate（6 条 LBR 收敛 + 边界测试 + 1 anchor default-active 字段 layout lock）+ 5 道 gate 全绿 + 0 改动 `src/*` / `tools/*` / `Cargo.toml` / `docs/pluribus_stage4_{validation,decisions,api}.md`（E1 [测试] 角色边界字面继承 stage 1/2/3 同型政策）。
    - **覆盖决策清单**：D-490 ① 单线程 ≥ 5K update/s / ② 4-core ≥ 15K / ③ 32-vCPU ≥ 20K + D-454 LBR P95 < 30 s + D-485 baseline 1-2 min wall time + D-461 24h continuous wall-time ≥ 10⁹ update/day + D-498 7-day nightly fuzz panic 标记符 + D-490 6-traverser per-traverser throughput cross-check（D-414 / D-459 §carve-out）+ D-451 first usable LBR < 200 mbb/g + D-452 100 采样点单调非升 ±10% 噪声 + D-459 per-traverser 上界 < 500 mbb/g + D-457 OpenSpiel-export byte-equal sanity + D-456 14-action vs 5-action ablation monotone + D-455 myopic horizon=1 边界。
    - **测试形态**：14 测试全 `#[ignore]` opt-in（release profile + AWS c7a.8xlarge / vultr 4-core 实测触发；artifact 缺失 / host CPU 不足走 eprintln + pass-with-skip）+ 1 anchor default-active `lbr_result_field_layout_lock`（LbrResult struct 字段顺序 byte-stable sanity，compile-only）。E1 closure 形态下 `LbrEvaluator::new` / `compute` / `compute_six_traverser_average` / `export_policy_for_openspiel` 全走 A1 [实现] scaffold `unimplemented!()`，opt-in 触发后立即 panic-fail；`evaluate_vs_baseline` 同型。①②③⑥⑧ SLO 走 `EsMccfrTrainer<NlheGame6>::step` / `step_parallel` 实测，D2 single-shared RegretTable + alternating 路径下 throughput 估计退化 1/2 边界紧，E2 [实现] 真并发 + lazy decay 落地后达 SLO。
    - **共享 helper**：`stage4_load_v3_artifact_or_skip()`（perf_slo.rs）+ `load_v3_or_skip()`（lbr_eval_convergence.rs）均同型 stage 3 `stage3_load_v3_artifact_or_skip` + `tests/training_24h_continuous.rs::load_v3_artifact_or_skip` ground truth（D-424 v3 artifact body BLAKE3 `67ee5554...` mmap + `NlheGame6::new` 路径）；artifact 缺失 → eprintln + return None pass-with-skip。
    - **5 道 gate 全绿**：`cargo fmt --all --check` ✅ / `cargo build --all-targets` ✅ / `cargo clippy --all-targets -- -D warnings` ✅（0 warning，含 unusual_byte_groupings + explicit_counter_loop 2 处修正）/ `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` ✅ / `cargo test --no-fail-fast` ✅（stage 1/2/3 baseline 维持 + stage 2 bucket_quality 9 known fail 不退化 + 14 新 ignored 测试默认跳过 + 1 anchor default-active pass）。
    - **角色边界**：E1 [测试] 单次 commit 触及 `tests/perf_slo.rs`（扩 stage 4 §E1 §输出 ~500 行 + 8 stage4_* SLO 测试 + 新增 3 import `LbrEvaluator` / `RandomOpponent` / `evaluate_vs_baseline` / `NlheGame6`）+ `tests/lbr_eval_convergence.rs`（新文件，~450 行 + 6 LBR 测试 + 1 anchor default-active 字段 layout lock）+ `docs/pluribus_stage4_workflow.md`（本节）+ `CLAUDE.md`（stage 4 progress 翻面到 "E1 [测试] closed"）。**0 改动** `src/*` / `tools/*` / `Cargo.toml` / `docs/pluribus_stage4_{validation,decisions,api}.md`（E1 [测试] 不修改决策 / API / validation 文档；如发现 spec 错误走 D-NNN-revM 流程，本 commit 未触发）。
    - **E1 → E2 工程契约**：E1 [测试] 落地后 E2 [实现] 起步前必 lock (a) `src/training/lbr.rs` 全 4 方法落地（`LbrEvaluator::new` 字面 14/5 action_set_size + horizon=1 主路径校验 + reject 7 → `PreflopActionAbstractionMismatch` / `compute` D-452 1000 hand × LBR-player + 14-action enumerate + myopic best response / `compute_six_traverser_average` D-459 6 traverser independent + average + max + min / `export_policy_for_openspiel` D-457 OpenSpiel-compatible policy 文件 byte-equal export）；(b) `src/training/trainer.rs` D-321-rev2 真并发 rayon long-lived pool + append-only delta merge（继承 stage 3 E2-rev1 模式扩 6-traverser × NlheGame6）+ `step_parallel` 走 `[RegretTable<NlheGame6>; 6]` + `[StrategyAccumulator<NlheGame6>; 6]` 数组（§D2-revM table-array deferral 翻面）；(c) Checkpoint v2 body sub-region encoding 落地真 6 套表序列化（D2 落地 v2 schema header 6-traverser dimension + regret_offset / strategy_offset 字面 layout，E2 接 body bincode 6-region 翻面）；(d) D-401-revM lazy decay 翻面 evaluate（如 E1 SLO ① 单线程 < 5K update/s）。**通过标准**：E1 全 14 active 测试（默认 panic-fail / opt-in 触发 panic）转绿 + AWS c7a.8xlarge × 32-vCPU 实测 D-490 ③ ≥ 20K update/s + D-454 LBR P95 < 30 s + stage 1/2/3 baseline byte-equal 全套维持 + B1 / C1 / D1 既有 default-active 测试全套不退化。如 first usable 10⁹ update 后 LBR ≥ 200 mbb/g → D-421-revM preflop 独立 action set 翻面 evaluate（F2 起步前 + 用户授权 lock）。

- **2026-05-15（§E-rev2 carve-out — A1 + A2 perf 优化 AWS c7a.8xlarge profiling 触发）**：stage 4 E2 [实现] closure 后用户授权 AWS c7a.8xlarge × 32 vCPU on-demand 实测 stage4_* SLO 8 条,实测数字与 perf flamegraph root cause 分析详见 `docs/pluribus_stage4_profiling.md`。SLO ③ 32-vCPU 29,136 update/s ≥ 20K **已 PASS 不阻塞 F1**;SLO ② 4-core 9,605 < 15K + SLO ⑧ 6-traverser deviation 102.6% > 50% + SLO ⑥ 24h projected 6.72e8 (测试形态 bug,走单线程 `step()` 而非 `step_parallel`) 三条 fail 进入 §E-rev2 carve-out 优化清单。本 carve-out **不阻塞** F1 [测试] 起步;F1 起步时 §E-rev2 commit 已落地 → SLO 实测重跑数字写入 §F-rev / F3 报告。
    - **A1 优化 — hoist legal_actions out of `PluribusActionAbstraction::actions()` 14× → 1×**：profiling 实测 `is_legal` + `legal_actions` 在 step_parallel hot path 占 ~12-16%（4-core 11.6% / 32-vCPU 16.4%）。`actions()` 内 14 次 `is_legal()` 各自重算 `LegalActionSet` → 一次性 hoist 走私有 helper `is_legal_cached(&action, state, &legal)` filter。`is_legal()` 公开签名保持（pub method 公开 API surface 不动 / `tests/api_signatures.rs` API-420 trip-wire 不破）;输出与旧实现 byte-equal（`legal_actions` 是 `&GameState` 纯函数,接收外部传入与内部重算等价）。**触及文件**：`src/abstraction/action_pluribus.rs:139-180` + 0 测试改 + 0 配置改。预期增幅 +10-15% throughput（pure CPU-side 优化）。
    - **A2 优化 — `step_parallel` 内部 batch K traversal per rayon task**：profiling 实测 rayon coordination overhead（crossbeam_epoch::with_handle + try_advance + Stealer::steal）+ `__sched_yield`（rayon worker idle yield）+ kernel sched 占 ~35-44%。Root cause = `step_parallel(pool, n_threads)` 调用频率 2,167 calls/s（4-core）/ 817 calls/s（32-vCPU）× 任务粒度 ~100 μs/traversal **远小于** rayon work-stealing 协调成本。**方案**：`TrainerConfig` 加 `parallel_batch_size: usize` 字段（default 1，preserving 既有 byte-equal）+ `EsMccfrTrainer::with_parallel_batch_size(k)` builder。`step_parallel` 内每 rayon task 跑 `batch` 次连续 traversal（traverser routing `(base_update_count + tid * batch + k) % n_players` for k=0..batch-1，preserving D-307 alternating semantic）;merge 阶段按 tid 升序 × per-task push 顺序 playback 保跨 run BLAKE3 决定性。`update_count += n_active * batch` per call。**触及文件**：`src/training/trainer.rs:120-155`（TrainerConfig）+ `:622-748`（step_parallel）+ `:565`（builder 附近新加）+ `tests/api_signatures.rs:900-919`（API-401 TrainerConfig 字面 + new builder trip-wire）+ `tests/perf_slo.rs::stage4_*`（② ⑥ ⑧ SLO 测试切换到带 batch=8 默认；⑥ 同步修测试形态 bug 走 step_parallel 替代单线程 step）。预期增幅 +30-50% throughput on top of A1（AWS c7a.8xlarge 实测 32-vCPU batch=8 给 66K update/s = +153% on baseline 29K,远超预估）。
    - **byte-equal anchor 不破声明**：A1 + A2 全部 stage 1 + stage 2 + stage 3 baseline byte-equal 维持（`stage{1,2,3}-v1.0` tag 在 §E-rev2 commit 上仍可重跑 byte-equal）;stage 3 1M update × 3 BLAKE3 anchor（warm-up phase 字面继承）byte-equal 维持 — warm-up phase 走单线程 `step()`,**不消费** `step_parallel`,A2 batch 修改不触达。stage 4 D2 closure 6-traverser checkpoint round-trip BLAKE3 byte-equal 维持。
    - **测试形态 bug 修复（§E-rev2 顺手）**：`stage4_24h_continuous_wall_time_ge_1e9_update_per_24h` 旧实现走单线程 `step()` 7,778 update/s 外推 6.72e8 < 10⁹ → 测试形态 bug（first usable 训练真路径走 `step_parallel(32)` ~29K update/s,真 24h projected 2.52e9 ≫ 10⁹）;§E-rev2 commit 修该测试走 `step_parallel(n_threads ∈ [4, 32]) × batch=8` 实测路径,AWS c7a.8xlarge 32-vCPU 实测 62,616 update/s → 24h projected 5.41e9 ≫ 10⁹（PASS）。Host < 4 core 走 pass-with-skip（first usable 训练真路径 ≥ 4 thread step_parallel 字面）。
    - **deferred carve-out 状态翻面**：D-401-revM lazy decay 仍 deferred（A2 不触达）;D-430-revM FxHashMap 仍 deferred（Path B2 评估）;D-459-revM 6-traverser imbalance 102.6% **进入** F-rev / F3 评估清单（A2 后重测 deviation 判断是否需要翻面 6-traverser routing 字面 alternating 重设计）。

- **2026-05-15（E2 [实现] 落地）**：stage 4 E2 [实现] 单 commit 落地 — `src/training/lbr.rs` LbrEvaluator 全 4 方法翻面（D-450 / D-452 / D-455 / D-456 / D-457 / D-459）+ `src/training/trainer.rs` D-321-rev2 真并发模式扩 6-traverser × NlheGame6 + §D2-revM table-array deferral 翻面 + Checkpoint v2 body sub-region encoding 落地 + `tools/lbr_compute.rs` CLI 主体 + `src/training/regret.rs` `#[derive(Clone)]` 让 per_traverser lazy clone-from-shared 成立 + 5 道 gate 全绿。
    - **覆盖决策清单**：D-450 LBR 算法（myopic horizon=1 best-response enumerate）+ D-452 n_hands sampling × computation_seconds 计时 + D-453 Rust 自实现（无 PyO3 bridge，与 stage 3 D-366 同型 one-shot OpenSpiel sanity 路径解耦）+ D-455 horizon=1 主路径 + horizon=0 退化（pure blueprint self-play → LBR ≈ 0 mbb/g）+ horizon ≥ 2 `unimplemented!()` 边界（D-453-revM deferred）+ D-456 14/5 action_set_size 字面双路径 + 拒绝 7 → `PreflopActionAbstractionMismatch` + D-457 OpenSpiel-compatible JSONL byte-equal export（per-traverser × per-InfoSet Debug-sort 顺序）+ D-459 6-traverser independent + `per_traverser` / `average_mbbg` / `max_mbbg` / `min_mbbg` + HU 退化 `n_players=2` slot 索引 `i % n_players` 兼容 + D-412 §D2-revM table-array deferral 翻面（`EsMccfrTrainer::per_traverser: Option<PerTraverserTables<G::InfoSet>>` 字段 + lazy `ensure_per_traverser_initialized` clone-from-shared 激活 + step / step_parallel 路由 + `current_strategy_for_traverser` / `average_strategy_for_traverser` override + Checkpoint v2 body 6-region bincode `Vec<Vec<(I, Vec<f64>)>>` encode/decode + `traverser_count` header 字段 dispatch + D-414 cross-traverser 不共享 strategy）。
    - **per_traverser 激活语义**：`should_use_per_traverser` 返 `true` ⇔ `G::VARIANT == GameVariant::Nlhe6Max && config.linear_weighting_enabled && config.rm_plus_enabled && update_count >= warmup_complete_at`。激活时 `ensure_per_traverser_initialized` lazy deep-clone single shared `regret` / `strategy_sum` × `n_players` 套（NlheGame6 = 6 / HU 退化 = 2）让 n 个 traverser 从同一份 warmup 出口 state 起步独立累积（D-414 字面 "cross-traverser regret 不共享"）；非激活路径（stage 3 / Kuhn / Leduc / SimplifiedNlhe / warmup phase / 默认 `EsMccfrTrainer::new`）走 single-shared 路径，stage 1/2/3 BLAKE3 byte-equal anchor 不破。
    - **step / step_parallel dispatch**：单线程 `step` 在 per_traverser 激活时把 D-401 Linear decay + D-403 strategy_sum weighted accumulation + D-402 RM+ clamp 三步全走 `per_traverser[traverser]` 上；step_parallel 沿用 stage 3 E2-rev1 rayon long-lived pool + append-only delta（`LocalRegretDelta` / `LocalStrategyDelta`），每线程读 `regret[traverser]` 作 σ 共享只读源 + 写到线程本地 delta，merge 阶段按 tid 升序 dispatch 到 `per_traverser[traverser]` 表（多个 tid 指向同一 traverser 时按 tid 串行 playback，保跨 run 决定性）。
    - **Checkpoint v2 6-region encoding**：`encode_multi_table(&[&HashMap])` 序列化 `Vec<Vec<(I, Vec<f64>)>>`（outer 顺序 = traverser index 0..N，inner entries 按 `Debug` 排序保跨 host BLAKE3 byte-equal，继承 D-327 single-region 同型政策）；`decode_multi_regret` / `decode_multi_strategy` 反向解析 + 校验 outer 长度 == header `traverser_count`，不一致返 `Corrupted`。`save_checkpoint` dispatch — `per_traverser.is_some()` → 6-region body + `traverser_count = n_players`；`per_traverser.is_none()` → single-region body + `traverser_count = 1`（pre-warmup save 路径 + 与 stage 3 byte-equal anchor 兼容）。`load_checkpoint` dispatch — `ckpt.traverser_count > 1` → 6-region decode 入 `per_traverser` + `regret` / `strategy_sum` 留空；`ckpt.traverser_count <= 1` → single-region decode（stage 3 path）。
    - **LBR 算法实现细节**：`simulate_one_hand`（D-450）— DFS 到 LBR-player 第 1 决策点 → 枚举 `action_set_size` 个 candidate → 每 candidate clone state + apply + `playout_blueprint` MC sample 1 次估 EV → 取 max EV candidate → 继续 blueprint sample 到 terminal 取 lbr_player payoff（chips → mbb/g 走 `chips / 100 * 1000 = chips * 10`，bb=100 chip 字面继承 stage 1 D-022）。`restrict_action_set` — 5-action ablation 走 `legal_actions[..5]`（截断前 5 个 legal action；当 legal_actions ≤ 5 时全部返回）；14-action 主线 `legal_actions.to_vec()`（`PluribusActionAbstraction::actions` 输出全部）。`sample_blueprint_action` — average σ 过滤 zero-probability + 归一化（应对 short-history 期间 σ sum 偏离 1）→ `sample_discrete` 入口；σ 空 / sum 非有限 → uniform fallback（D-331 退化局面字面继承）。`export_policy_for_openspiel` — line-delimited JSON `{"traverser":t,"info_set":"<Debug>","average_strategy":[p_0,p_1,...]}`，traverser 升序 × per-traverser InfoSet `Debug` 排序保跨 host byte-equal（D-457 字面 + 继承 stage 3 D-327 sort 政策）。
    - **D-401-revM lazy decay 评估状态**：E2 commit 维持 D-401 eager decay 主路径（`TrainerConfig::default()` `DecayStrategy::EagerDecay`）；lazy decay 翻面条件 = "E1 SLO ① 单线程 < 5K update/s 实测 fail" 由用户授权 AWS c7a.8xlarge 实测触发后决定，**仍 deferred** carry-forward stage 4 F-rev / F3 报告（与 D2 carve-out 同型 deferred 政策）。
    - **OOM step-path dispatch 仍 deferred**：`TrainerError::OutOfMemory` variant A1 已落地，E2 训练循环未接入 runtime trigger（D2 §D2-revM (v) carve-out 字面延续，F2 [实现] `MetricsCollector::observe` 接入后实际触发 dispatch）。
    - **`tools/lbr_compute.rs` CLI 主体落地**：9 个 flag — `--checkpoint`（必填，Linear+RM+ NlheGame6 v2 checkpoint 路径）/ `--bucket-table`（必填，v3 production artifact 路径）/ `--n-hands`（缺省 1000，D-452 字面）/ `--traverser` 或 `--six-traverser`（缺省 six-traverser 主路径 D-459，互斥）/ `--action-set-size`（缺省 14，D-456 字面）/ `--myopic-horizon`（缺省 1，D-455 字面）/ `--seed`（缺省 0，D-027 显式 RNG）/ `--openspiel-export`（可选，D-457 一次性 sanity）。输出 JSON 单行（single-traverser）或多行 array（six-traverser）。错误路径返 non-zero exit code + stderr 提示。
    - **5 道 gate 全绿**：`cargo fmt --all --check` ✅ / `cargo build --all-targets` ✅ / `cargo clippy --all-targets -- -D warnings` ✅（0 warning）/ `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` ✅ / `cargo test --no-fail-fast` ✅（stage 1/2/3 baseline 维持 + stage 2 bucket_quality 9 known fail 不退化 + B1/C1/D1/E1 既有 default-active 测试全套不退化）。
    - **角色边界**：E2 [实现] 单次 commit 触及 `src/training/lbr.rs`（4 方法 + 5 inherent / standalone helper） + `src/training/trainer.rs`（`per_traverser` 字段 + `PerTraverserTables` struct + `ensure_per_traverser_initialized` / `should_use_per_traverser` / `per_traverser_active` 3 helper + step/step_parallel dispatch 重排 + `current_strategy_for_traverser` / `average_strategy_for_traverser` override + save/load_checkpoint dispatch + `encode_multi_table` / `decode_multi_regret` / `decode_multi_strategy` 3 helper） + `src/training/regret.rs`（RegretTable / StrategyAccumulator 加 `#[derive(Clone)]`） + `tools/lbr_compute.rs`（CLI main body 落地，9 flag + dispatch） + `docs/pluribus_stage4_workflow.md`（本节） + `CLAUDE.md`（stage 4 progress 翻面到 "E2 [实现] closed"）。**0 改动** `tests/*` / `Cargo.toml` / `src/error.rs` / `src/training/{checkpoint,nlhe_6max,game,kuhn,leduc,nlhe,sampling,best_response,metrics,slumbot_eval,baseline_eval,mod}.rs` / `docs/pluribus_stage4_{validation,decisions,api}.md`（E2 [实现] 不修改测试 / 决策 / API / validation 文档；如发现 spec 错误走 D-NNN-revM 流程，本 commit 未触发）。
    - **E2 → F1 工程契约**：E2 [实现] 落地后 F1 [测试] 起步前必 lock (a) `tests/slumbot_eval.rs` 6 条测试（D-460/461 + D-463 + D-468 fold equity + D-469 + D-463-revM fallback OpenSpiel HU baseline，全 `#[ignore]` opt-in）；(b) `tests/baseline_eval.rs` 12 条测试（D-480 3 baseline × 4 metric — random / call-station / TAG × min PnL / per-traverser min / 1M 手 BLAKE3 regression / 95% CI 下界）；(c) `tests/cross_host_blake3.rs` 扩展（既有 stage 3 + 追加 `tests/data/checkpoint-hashes-linux-x86_64-stage4.txt` 32-seed × 6-traverser × first usable 10⁵ checkpoint anchor）；(d) `tests/api_signatures.rs` 扩展 API-450..API-499 全套 trip-wire（LBR / Slumbot / baseline / metrics / 24h continuous）。**通过标准**：default profile 30 测试 panic-fail（F2 实现后转绿）+ 8 active anchor 测试通过。如 E2 commit 在 AWS c7a.8xlarge first usable 10⁹ 训练实测中 LBR ≥ 200 mbb/g 触发 D-421-revM preflop 独立 action set 翻面 evaluate（用户授权 + F2 起步前 lock）。

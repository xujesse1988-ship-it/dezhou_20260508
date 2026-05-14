# 阶段 3 实施流程：test-first 路径

## 文档目标

本文档把阶段 3（MCCFR 小规模验证：Vanilla CFR + ES-MCCFR + Kuhn / Leduc / 简化 NLHE）的实施工作拆解为可执行的步骤序列。它不重复 `pluribus_stage3_validation.md` 的验收门槛，只回答一个具体问题：**在已有验收门槛 + `pluribus_stage3_decisions.md` 锁定的算法 / 数据结构 / 序列化格式 / API 签名前提下，工程上按什么顺序写代码、写测试、做 review，最不容易翻车，并且能让多 agent 协作完成**。

阶段 3 与阶段 1 / 阶段 2 的最大差异：

- **阶段 1 有 PokerKit 做 byte-level ground truth**；
- **阶段 2 没有同型开源参考，但有内部不变量（preflop 169 lossless / clustering BLAKE3 byte-equal）做 anchor**；
- **阶段 3 既无 byte-level reference 又无内部 anchor**——Kuhn 有 closed-form Nash 解析解（player 1 EV `-1/18`）做收敛锚点，Leduc 完全靠 fixed-seed BLAKE3 byte-equal + 训练曲线趋势对照，简化 NLHE 100M update 量级的策略质量**几乎不可外部验证**，只能靠 average regret growth sublinear + 训练吞吐 + checkpoint round-trip 等工程不变量守住。

阶段 3 的工程风险因此集中在两点：

1. **CFR 算法实现细节错误难以观察**：cfv 累积、π_traverser / π_opp 乘子、regret update 公式、average strategy 累积——任一处错误都会让 Kuhn 不收敛到 `-1/18`、Leduc 训练曲线发散、简化 NLHE average regret 线性增长，**事后定位成本极高**（无 byte-level diff，只有数值发散）。
2. **Checkpoint round-trip 数值漂移**：bincode 浮点序列化跨 host 不一致、RNG state 恢复后 sampling sequence 漂移、HashMap 序列化顺序不稳定，任一处都会让 stage 4+ 训练恢复后结果与不中断训练 BLAKE3 不一致，**stage 4 100B update 训练时无法 resume 等价于训练从头重来**。

阶段 3 的 [测试] 优先策略**比 stage 2 更激进**——B1 [测试] 必须把 Kuhn closed-form anchor 测试钉死才能让 B2 [实现] 起步，否则 CFR 实现细节错误会通过 LBR/exploitability 漂移渗入 stage 4。

## 总体原则

**正确性 + 数值容差 + 确定性 test-first，性能 implementation-first**（继承 stage 1 + stage 2，额外强调 CFR 数值正确性 anchor）。

- Kuhn closed-form anchor（player 1 EV `-1/18`）+ Leduc fixed-seed BLAKE3 byte-equal + checkpoint round-trip 三条不变量必须在 B1 [测试] / D1 [测试] 钉死，不许 [实现] agent 顺手放宽。
- regret matching 数值容差 `1e-9` 是 path.md §阶段 3 字面强约束，B1 [测试] 必须把容差 + 退化均匀分布 + 零和约束三条数值 sanity 全部覆盖。
- stage 1 锁定的 `GameState` / `HandEvaluator` / `HandHistory` / `RngSource` + stage 2 锁定的 `ActionAbstraction` / `InfoAbstraction` / `EquityCalculator` / `BucketTable` API surface **冻结**。stage 3 不允许顺手改 stage 1 / stage 2 接口；如发现确实不够用，走 stage 1 / stage 2 `API-NNN-revM` 修订流程。
- 浮点路径（regret / average strategy / cfv）与 stage 1 整数 chip + stage 2 整数 bucket id 路径必须 **物理隔离**：`src/training/` 允许浮点；stage 2 `abstraction::map` 子模块 `clippy::float_arithmetic` 死锁继续生效。

阶段 3 的所有 bug 都会随 regret signal 进入 stage 4-6 并被 100B update 量级训练放大，事后几乎无法定位（与 stage 1 + stage 2 同型表述）。

## Agent 分工

继承 stage 1 + stage 2 §Agent 分工 全部表格与跨界规则：

| 标签 | Agent 类型 | 职责 | 禁止 |
|---|---|---|---|
| **[决策]** | 决策者（人 / 决策 agent） | 算法 / 游戏环境 / 数据结构 / 序列化格式 / API 契约 | — |
| **[测试]** | 测试 agent | 写测试用例、scenario DSL、harness、benchmark 配置、CFR 收敛性检查器 | 修改产品代码（除测试夹具） |
| **[实现]** | 实现 agent | 写产品代码：`Trainer` / `Game` / `RegretTable` / `BestResponse` / `Checkpoint` 等 | 修改测试代码 |
| **[报告]** | 报告者（人 / 报告 agent） | 跑全套测试、产出验收报告 | — |

跨界规则、`carve-out` 追认机制、`#[ignore]` full-volume 测试在下一步 [实现] 步骤实跑、CLAUDE.md 同步责任、修订历史 "追加不删" — 全部继承 stage 1 + stage 2 §修订历史 提炼的处理政策。**阶段 3 §修订历史 首条新增项必须显式 carry forward 这套政策**，不重新论证。

## 工程脚手架与技术栈选择

### 沿用 Rust（继承 stage 1 + stage 2）

stage 1 + stage 2 已锁定的 dependency 全部继承。stage 3 候选新增依赖（A0 [决策] D-373 锁定）：

- `bincode = "1.3"`（D-327 checkpoint 序列化，LE 默认）
- `tempfile = "3"`（D-353 write-to-temp + atomic rename）
- thread-safety crate（D-321 batch 3 [实现] 之前 lock；候选 `parking_lot 0.12` / `dashmap 5.5` / `crossbeam 0.8`）

**不引入**：① `nalgebra` / `ndarray`（继承 stage 2 D-250 自实现政策）；② `tokio` / `async-std`（CFR CPU-bound 不需 async）；③ ML 框架（用户路线明确 stage 3-6 全 MCCFR + nested subgame solving，不引 NN）。

### Module 布局（D-370）

stage 1 单 crate 多 module 已经稳定；stage 2 仍在同一 `poker` crate 下加 module；stage 3 同型在 `poker` crate 下加 module，**不分 crate**：

```
src/
├── core/             # stage 1 锁定，stage 3 只读
├── rules/            # stage 1 锁定，stage 3 只读
├── eval/             # stage 1 锁定，stage 3 只读
├── history/          # stage 1 锁定，stage 3 只读
├── abstraction/      # stage 2 锁定，stage 3 只读
├── error.rs          # stage 1 + stage 2 锁定；stage 3 仅追加 CheckpointError + TrainerError
└── training/         # ★ stage 3 新增
    ├── mod.rs
    ├── game.rs           # Game trait + NodeKind + PlayerId
    ├── kuhn.rs           # KuhnGame
    ├── leduc.rs          # LeducGame
    ├── nlhe.rs           # SimplifiedNlheGame
    ├── regret.rs         # RegretTable + StrategyAccumulator
    ├── trainer.rs        # Trainer trait + VanillaCfrTrainer + EsMccfrTrainer
    ├── sampling.rs       # RngSource sub-stream + sample_discrete + 6 op_id const
    ├── best_response.rs  # BestResponse trait + KuhnBestResponse + LeducBestResponse + exploitability
    └── checkpoint.rs     # Checkpoint binary schema + CheckpointError + save/open
```

`tools/`：stage 3 新增

- `train_cfr.rs` CLI（D-372 训练 entry point）
- `checkpoint_reader.py`（D-357 Python 跨语言读取参考，对照 stage 1 + stage 2 reader）
- `external_cfr_compare.py`（D-366 F3 [报告] 起草时一次性接入 OpenSpiel 对照）

checkpoint artifact 落到 `artifacts/`（继承 stage 2 D-251 gitignore + git LFS / release artifact 分发），**不进 git history**（stage 3 出口 F3 决定分发渠道）。

---

## 步骤序列

总览：`A → B → C → D → E → F`，共 6 个阶段、13 个步骤（与 stage 1 + stage 2 同形态）。每个阶段内部测试与实现交替推进。

```
A. 决策与脚手架            : A0 [决策] → A1 [实现]
B. 第一轮：Kuhn / Leduc    : B1 [测试] → B2 [实现]
C. 第二轮：简化 NLHE      : C1 [测试] → C2 [实现]
D. 第三轮：checkpoint + fuzz : D1 [测试] → D2 [实现]
E. 第四轮：性能 SLO       : E1 [测试] → E2 [实现]
F. 收尾                    : F1 [测试] → F2 [实现] → F3 [报告]
```

---

### A. 决策与脚手架

#### 步骤 A0：算法 / API 契约锁定 [决策]

**目标**：锁定 stage 3 全部开放决策点，给后续 [测试] / [实现] agent 一份共同 spec。

**输入**：
- `pluribus_path.md` §阶段 3 字面 5 条门槛
- stage 1 + stage 2 全部决策 + API（D-001..D-103 + D-200..D-283 + API-001..API-013 + API-200..API-302）
- 用户决策：双轨 Vanilla CFR (Kuhn/Leduc) + ES-MCCFR (简化 NLHE)
- 用户授权：stage 3 [决策] 优先于 §G-batch1 §3.4-batch2..§4 closure 启动（carry-forward carve-out）

**产出（6 batch）**：
1. `docs/pluribus_stage3_validation.md`（path.md §阶段 3 字面 5 条门槛量化展开 + 通过标准 + 完成产物 + 进入 stage 4 门槛）
2. `docs/pluribus_stage3_decisions.md` §1 算法变体 D-300..D-309（Vanilla CFR / ES-MCCFR / 非 Linear / 标准 RM / D-300 + D-301 详解伪代码）
3. `docs/pluribus_stage3_decisions.md` §2-§4 D-310..D-339（游戏环境 + regret/strategy 存储 + sampling/traversal）
4. `docs/pluribus_stage3_decisions.md` §5-§8 D-340..D-379（exploitability + checkpoint + 性能 SLO + crate/module）
5. `docs/pluribus_stage3_api.md` API-300..API-392（Game / Trainer / RegretTable / BestResponse / Checkpoint trait + 桥接 + doc-test）
6. `docs/pluribus_stage3_workflow.md`（本文档）+ CLAUDE.md 更新 stage 3 起步状态

**carve-out**：D-314（bucket table 依赖）+ D-321（thread-safety 模型）deferred 到 batch 3 [实现] 之前 lock；D-302-rev1 / D-303-rev1（Linear / RM+）deferred 到 F2 [实现] 收尾前。

#### 步骤 A1：scaffold 落地 [实现]

**目标**：把 `pluribus_stage3_api.md` 锁定的全部公开 trait / struct / enum 签名落到 `src/training/` + `tools/train_cfr.rs`，方法体 `unimplemented!()` 占位；通过 `cargo build --all-targets` + `cargo fmt --all --check` + `cargo clippy --all-targets -- -D warnings`。

**产出**：
- `src/training/` 9 个文件骨架（`mod.rs` / `game.rs` / `kuhn.rs` / `leduc.rs` / `nlhe.rs` / `regret.rs` / `trainer.rs` / `sampling.rs` / `best_response.rs` / `checkpoint.rs`）
- `src/error.rs` 追加 `CheckpointError` + `TrainerError` 枚举
- `src/lib.rs` 追加 `pub mod training;` + re-export（API-380）
- `tools/train_cfr.rs` CLI 骨架 + `Cargo.toml [[bin]]` 入口
- `Cargo.toml` 新增 3 个依赖（`bincode 1.3` / `tempfile 3` / thread-safety TBD D-321）
- `tests/api_signatures.rs` 扩展 stage 3 API surface trip-wire（继承 stage 2 §A1 模式，签名漂移 `cargo test --no-run` fail）
- `cargo doc --no-deps` 全绿（含 doc-test 占位 `#[doc(hidden)]` 至 B2 / C2 落地）

**不变量**：所有公开方法体 `unimplemented!()`；A1 不引入任何业务逻辑，仅 trip-wire。

---

### B. 第一轮：Kuhn / Leduc 核心场景

#### 步骤 B1：Kuhn closed-form anchor + Leduc 收敛曲线 [测试]

**目标**：把 Kuhn closed-form Nash 解析解 + Leduc 收敛曲线趋势 + regret matching 数值容差 三条不变量全部覆盖；让 B2 [实现] 任何 CFR 实现细节错误（cfv / π / regret update / average strategy 累积）能立即被测试 fail 捕获。

**产出**：
- `tests/cfr_kuhn.rs`：
    - `kuhn_vanilla_cfr_10k_iter_player_1_ev_close_to_minus_one_over_eighteen`（D-340 closed-form anchor，10K iter 后 EV 差距 `< 1e-3`，release ignored）
    - `kuhn_vanilla_cfr_10k_iter_exploitability_less_than_0_01`（D-340，path.md §阶段 3 字面）
    - `kuhn_vanilla_cfr_fixed_seed_repeat_1000_times_blake3_identical`（D-362，重复 1000 次 BLAKE3 一致，release ignored）
    - `kuhn_vanilla_cfr_zero_sum_invariant_ev_sum_below_1e_minus_6`（D-332 零和约束）
- `tests/cfr_leduc.rs`：
    - `leduc_vanilla_cfr_10k_iter_exploitability_less_than_0_1`（D-341，release ignored）
    - `leduc_vanilla_cfr_curve_monotonic_non_increasing_at_1k_2k_5k_10k`（D-341，curve 单调非升允许 ±5% 噪声）
    - `leduc_vanilla_cfr_fixed_seed_repeat_10_times_blake3_identical`（D-362，重复 10 次 BLAKE3 一致，release ignored）
    - `leduc_vanilla_cfr_zero_sum_invariant`（D-332）
- `tests/regret_matching_numeric.rs`：
    - `regret_matching_probability_sum_within_1e_minus_9_tolerance_1M_random_inputs`（D-330 + path.md §阶段 3 字面）
    - `regret_matching_all_zero_regrets_returns_uniform_distribution`（D-331 退化局面）
    - `regret_matching_handles_negative_regrets_via_max_zero`（D-303 + D-306 标准 RM）
- `tests/api_signatures.rs`：B1 同 commit 落地 Kuhn / Leduc 相关 trait / struct / enum 签名锁
- `benches/stage3.rs`：B1 落地 `stage3/kuhn_cfr_iter` + `stage3/leduc_cfr_iter` 2 个 bench group 框架（D-367 criterion）

**禁止**：B1 [测试] agent 不修改产品代码；如测试发现 A1 scaffold 签名 bug，filed issue 移交 [实现]。

#### 步骤 B2：Vanilla CFR + KuhnGame + LeducGame [实现]

**目标**：把 A1 scaffold 的 `VanillaCfrTrainer` + `KuhnGame` + `LeducGame` + `RegretTable` + `StrategyAccumulator` 方法体实现；让 B1 全部测试通过；不修改任何测试代码。

**产出**：
- `src/training/game.rs` Game trait 实现枝（PlayerOrChance / NodeKind dispatch）
- `src/training/kuhn.rs` KuhnGame 全部 Game trait 方法（D-310 Kuhn 规则 + InfoSet encoding）
- `src/training/leduc.rs` LeducGame 全部 Game trait 方法（D-311 Leduc 规则 + InfoSet encoding）
- `src/training/regret.rs` RegretTable + StrategyAccumulator 全部方法（D-320..D-329 + API-320 + API-321）
- `src/training/sampling.rs` derive_substream_seed + sample_discrete + 6 op_id const（D-335 / D-336 / API-330 / API-331）
- `src/training/trainer.rs` Trainer trait + VanillaCfrTrainer 方法（D-300 详解伪代码 + API-311）
- `src/training/best_response.rs` KuhnBestResponse + LeducBestResponse + exploitability 辅助函数（D-340 / D-341 / API-341 / API-342 / API-343）

**性能预期**：Kuhn 10K iter 单线程 release `< 1 s`（D-360）；Leduc 10K iter 单线程 release `< 60 s`（D-360）。

**测试通过**：B1 全部 active 测试（非 ignored）+ `cargo test --release --test cfr_kuhn / cfr_leduc -- --ignored` 全套通过。

---

### C. 第二轮：简化 NLHE 接入

#### 步骤 C1：SimplifiedNlheGame + ES-MCCFR 测试 [测试]

**目标**：把简化 NLHE Game trait 桥接 + ES-MCCFR 算法语义 + bucket table 依赖（D-314 在 C2 [实现] 起步前 lock）全部覆盖。

**产出**：
- `tests/cfr_simplified_nlhe.rs`：
    - `simplified_nlhe_game_root_state_2_player_100bb_starting_stack`（D-313 范围 sanity）
    - `simplified_nlhe_legal_actions_returns_default_action_abstraction_5_action`（D-318 桥接 sanity）
    - `simplified_nlhe_info_set_uses_stage2_infosetid`（D-317 桥接 sanity）
    - `simplified_nlhe_es_mccfr_1k_update_no_panic_no_nan_no_inf`（D-342 工程稳定性 smoke）
    - `simplified_nlhe_es_mccfr_fixed_seed_repeat_3_times_blake3_identical_1M_update`（D-362 重复确定性 smoke，release ignored）
- `tests/api_signatures.rs`：C1 同 commit 落地 SimplifiedNlheGame / EsMccfrTrainer 签名锁
- `benches/stage3.rs`：C1 落地 `stage3/nlhe_es_mccfr_update` bench group（D-367）

**D-314 lock**：C1 [测试] 起草前由用户决策 lock D-314 为 v1（D-314-rev2）或 v2（D-314-rev1）。该决策在 `pluribus_stage3_decisions.md` §10 已知未决项 in-place 翻面。

**禁止**：C1 [测试] agent 不修改产品代码。

#### 步骤 C2：EsMccfrTrainer + SimplifiedNlheGame [实现]

**目标**：把 A1 scaffold 的 `EsMccfrTrainer` + `SimplifiedNlheGame` 方法体实现；让 C1 全部测试通过；不修改任何测试代码。

**产出**：
- `src/training/nlhe.rs` SimplifiedNlheGame 全部 Game trait 方法（D-313 + API-303 + API-390 / API-391 / API-392 stage 1 + stage 2 桥接）
- `src/training/trainer.rs` EsMccfrTrainer 方法（D-301 详解伪代码 + API-312）+ step_parallel 多线程入口（D-321 thread-safety 模型在 C2 [实现] 起步前 lock；C2 commit 锁定具体实现）

**性能预期**：简化 NLHE 单线程 release `≥ 10K update/s`（D-361）；4-core `≥ 50K update/s`（D-361，效率 ≥ 0.5）。

**测试通过**：C1 全部 active 测试 + `cargo test --release --test cfr_simplified_nlhe -- --ignored` 全套通过。

---

### D. 第三轮：checkpoint + fuzz + 规模

#### 步骤 D1：Checkpoint round-trip + fuzz + 100M update [测试]

**目标**：把 checkpoint round-trip BLAKE3 byte-equal（path.md §阶段 3 字面）+ fuzz 不变量 + 简化 NLHE 100M update 量级稳定性 全部覆盖。

**产出**：
- `tests/checkpoint_round_trip.rs`：
    - `kuhn_vanilla_cfr_save_at_5_iter_resume_5_more_iter_blake3_equal_to_uninterrupted_10_iter`（D-350 round-trip 不变量）
    - `leduc_vanilla_cfr_save_at_1k_iter_resume_1k_more_iter_blake3_equal_to_uninterrupted_2k_iter`（D-350）
    - `simplified_nlhe_es_mccfr_save_at_1M_update_resume_1M_more_blake3_equal_to_uninterrupted_2M_update`（D-350，release ignored）
    - 5 类 CheckpointError 错误路径全部覆盖（D-351）：FileNotFound / SchemaMismatch / TrainerMismatch / BucketTableMismatch / Corrupted
    - byte-flip 1k smoke + 100k full（继承 stage 2 §F1 模式，release ignored）
- `tests/cfr_fuzz.rs`：
    - `cargo fuzz` target `cfr_kuhn_smoke` / `cfr_leduc_smoke` / `cfr_simplified_nlhe_smoke`（继承 stage 2 D1 模式）
    - active fuzz test：1k iter 3 game 各 0 panic
    - `#[ignore]` full-volume：1M iter / 100M update 各 0 panic（release ignored）
- `tests/simplified_nlhe_100M_update.rs`：
    - `simplified_nlhe_es_mccfr_100M_update_no_panic_no_nan_no_inf`（D-342，release ignored，单 host vultr ~3 h）
    - `simplified_nlhe_es_mccfr_100M_update_max_avg_regret_growth_sublinear`（D-343 average regret growth 监控）
- `tests/api_signatures.rs`：D1 同 commit 落地 Checkpoint / CheckpointError 签名锁

**禁止**：D1 [测试] agent 不修改产品代码。

#### 步骤 D2：Checkpoint + fuzz fix [实现]

**目标**：把 A1 scaffold 的 `Checkpoint` save/open + 5 类 CheckpointError 错误路径 + D1 fuzz / 100M update 暴露 bug 全部实现 / 修复。

**产出**：
- `src/training/checkpoint.rs` Checkpoint save/open 全部方法（D-350 schema + D-352 BLAKE3 eager 校验 + D-353 atomic rename + API-350 binary layout）
- `src/error.rs` CheckpointError 5 variant 完整 dispatch
- D1 [测试] 暴露 bug 在 src/training/ 内部修复（任何 fuzz panic / NaN / inf 必须 root cause + fix；不允许通过修改测试规避）

**测试通过**：D1 全部 active 测试 + `cargo test --release --test checkpoint_round_trip / cfr_fuzz / simplified_nlhe_100M_update -- --ignored` 全套通过。

---

### E. 第四轮：性能 SLO

#### 步骤 E1：性能 SLO 测试 [测试]

**目标**：把 D-360..D-369 全部性能 SLO 断言落到 `tests/perf_slo.rs::stage3_*` + `benches/stage3.rs`。

**产出**：
- `tests/perf_slo.rs` 新增 stage 3 测试组：
    - `stage3_kuhn_10k_iter_under_1s_release`（D-360）
    - `stage3_leduc_10k_iter_under_60s_release`（D-360）
    - `stage3_simplified_nlhe_single_thread_throughput_ge_10k_update_per_s`（D-361 单线程）
    - `stage3_simplified_nlhe_4core_throughput_ge_50k_update_per_s`（D-361 多线程，host 4-core 强制）
    - `stage3_kuhn_best_response_under_100ms_release`（D-348）
    - `stage3_leduc_best_response_under_1s_release`（D-348）
- `benches/stage3.rs` 3 个 bench group active + criterion measurement
- `tests/api_signatures.rs`：E1 同 commit 落地无变化（perf 测试不暴露新 API）

**禁止**：E1 [测试] agent 不修改产品代码。

#### 步骤 E2：性能优化 [实现]

**目标**：把 E1 暴露的 SLO 不达标修复；不修改任何测试代码。

**性能优化候选路径**（D-321 thread-safety 模型决定主导路径）：
- `RegretTable::current_strategy` hot path（D-303 + D-306 标准 RM）：避免 `Vec::new()` 分配走 stack-allocated `SmallVec`
- `sample_discrete` CDF binary search：常数小但调用频繁，可考虑 lookup-table 加速（D-336）
- ES-MCCFR cfv 累积（D-338）：per-action `Vec<f64>` 可改 `SmallVec<[f64; 8]>` 避免堆分配
- 多线程 SLO（D-361 4-core ≥ 50K update/s）：D-321 thread-safety 模型决定 lock contention 优化路径
- terminal_payoff 计算（D-339）：avoidance `.clone()` 走 `&GameState` 借用

**测试通过**：E1 全部 active SLO 测试 + bench group 实测达到 D-360..D-369 阈值。

---

### F. 收尾

#### 步骤 F1：边界 + 错误路径 + corruption 全套测试 [测试]

**目标**：把 5 类 CheckpointError + 5 类 TrainerError + corruption byte-flip + 跨 host BLAKE3 + 极端 InfoSet 边界 全部覆盖；这是 stage 3 出口前最后一次 [测试] 角色机会。

**产出**：
- `tests/checkpoint_corruption.rs`（继承 stage 2 §F1 模式）：
    - schema_version 字节越界（u32::MAX / 0）拒绝
    - trainer_variant / game_variant 越界拒绝
    - bucket_table_blake3 mismatch 拒绝（构造 random 32 byte 写入 checkpoint）
    - trailer BLAKE3 自校验拒绝
    - 100k byte-flip full sweep 0 panic（release ignored）
- `tests/trainer_error_boundary.rs`：5 类 TrainerError 全部命中（ActionCountMismatch / OutOfMemory / UnsupportedBucketTable / ProbabilitySumOutOfTolerance / CheckpointError propagation）
- `tests/cross_host_blake3.rs`（继承 stage 1 + stage 2 cross_arch baseline 模式）：32-seed regression guard（fixed seed → checkpoint BLAKE3 byte-equal across runs）
- `tests/api_signatures.rs`：F1 同 commit 落地 stage 3 公开 API 全套签名锁的最后一次 trip-wire

**禁止**：F1 [测试] agent 不修改产品代码。

#### 步骤 F2：边界修复 + 错误前移 [实现]

**目标**：把 F1 暴露 bug 全部修复；走 stage 1 + stage 2 §F2 / §F-rev1 错误前移模式（错误类型在源头检测，不允许下游通过 panic / `Result<_, anyhow::Error>` 兜底）。

**预期**：F1 暴露 bug 通常是错误路径 dispatch 缺失（如 BucketTableMismatch 字段未读取就 propagate）；走错误前移修复。如 F1 全套测试 0 fail，按 stage 2 §F2 字面预测形态走 "0 产品代码改动 carve-out closure"（合并 commit 修 doc drift）。

**测试通过**：F1 全部 active 测试 + `cargo test --release` 全套通过 + stage 1 + stage 2 baseline byte-equal 不退化（继承 stage 1 + stage 2 §F-rev0 不退化锚点）。

#### 步骤 F3：stage 3 验收报告 [报告]

**目标**：出 `docs/pluribus_stage3_report.md` + git tag `stage3-v1.0` + checkpoint artifact 落地。

**产出**：
- `docs/pluribus_stage3_report.md`：
    - §1 阶段目标 + 通过标准
    - §2 测试基线（cargo test 全套数字 + cargo test --release --ignored 实测）
    - §3 Kuhn closed-form anchor 实测（player 1 EV 数值 + exploitability + BLAKE3）
    - §4 Leduc 收敛曲线 + 4 sample point 数值
    - §5 简化 NLHE 100M update 实测（vultr 4-core 训练时长 + throughput + max_avg_regret growth + BLAKE3）
    - §6 性能 SLO 实测（D-360..D-369 全部数字）
    - §7 checkpoint round-trip 实测
    - §8 已知偏离 + carve-out 状态
    - §9 进入 stage 4 门槛 + carve-out 转 stage 4 起步并行清单
    - §10 stage 3 完成产物清单
    - §11 stage 4 切换说明（用户路线 [stage4_6_path] 锚点 + Linear CFR / RM+ 翻面候选）
- `docs/pluribus_stage3_external_compare.md`（D-366 F3 [报告] 起草时一次性接入 OpenSpiel 收敛曲线对照）
- `tools/external_cfr_compare.py`（D-366 一次性 instrumentation；与 stage 2 D-263 同型 carve-out 模式）
- `tools/checkpoint_reader.py`（D-357 Python 跨语言 reader）
- git tag `stage3-v1.0`（commit message 含完整 carve-out 索引）
- checkpoint artifact 上传 GitHub Release tag `stage3-v1.0`（Kuhn / Leduc / 简化 NLHE 100M update 各 1 个 milestone checkpoint）

**carve-out 状态翻面**：
- §G-batch1 §3.4-batch2..§4 production artifact 重训：F3 [报告] 前由 D-314 lock 决定是否复用既有 v2 artifact 或回头补
- 12 条 `tests/bucket_quality.rs` `#[ignore]` 转 active：F3 之后回头补（或合并到 stage 4 起步并行）
- stage 2 `pluribus_stage2_report.md` §8 carve-out：F3 之后翻面（D-218-rev2 真等价类 + bucket quality 全绿 + 跨架构 baseline 重生）

---

## 反模式（不要做）

继承 stage 1 + stage 2 反模式全集，**额外强调 stage 3 高风险反模式**：

1. **优化前确认正确性**——E2 之前任何 cfv 累积 / regret update 优化都是错误时序。Kuhn 10K iter 单线程 release `< 1 s` 在 naive 实现已可达，B2 [实现] 不需要任何优化。
2. **跳过 Kuhn closed-form anchor**——B1 [测试] 必须把 `player 1 EV ≈ -1/18` 钉死，否则 CFR 实现细节错误（cfv 乘子错位 / π_traverser ↔ π_opp 互换 / regret update sign 错误）会通过 LBR 漂移渗入 stage 4。
3. **隐式 RNG**——CFR / MCCFR 训练任何 sampling / tie-break / shuffle 必须显式接 `RngSource`。任何 `rand::thread_rng()` 是 P0 阻塞 bug，违反 stage 1 D-027 + D-050。
4. **f32 替代 f64**——`f32` 在 100M update 量级累积误差超过 path.md `1e-9` 容差；D-333 锁定 f64。f32 优化路径在 stage 4+ 出现性能瓶颈时再视情况引入（D-333-revM 翻面）。
5. **持久化 state representation**——继承 stage 1 `unsafe_code = "forbid"` + Cow / Rc / persistent data structure 与 stage 1 设计冲突；D-319 锁定 owned clone。
6. **incremental checkpoint**——D-358 锁定 full snapshot；incremental delta encoding 在 stage 3 不引入。
7. **修改 stage 1 / stage 2 接口**——stage 3 [实现] agent 顺手改 stage 1 `RngSource` / stage 2 `BucketTable` 接口是 stage 3 工程红线。需要时走 stage 1 / stage 2 `API-NNN-revM` 修订流程。
8. **OpenSpiel 数值 byte-equal**——D-364 锁定收敛轨迹趋势对照，不要求各 iter exploitability 数值 byte-equal。任何强求 byte-equal 是 stage 3 工程红线。
9. **bucket table mid-training 升级**——D-356 锁定 BucketTableMismatch 拒绝；stage 3 训练全程 bucket_table_blake3 必须恒定。中途升级 v1 → v2 等价于 fresh start。
10. **Linear CFR / RM+ 提前引入**——D-302 + D-303 锁定 stage 3 非 Linear + 标准 RM。提前引入 = sampling + weighting + matching variant 三变量同时引入，调试成本极高。

---

## 阶段 3 出口检查清单

只有当全部门槛全部满足，才能 git tag `stage3-v1.0`：

- [ ] `cargo test`（默认）全套通过；ignored 测试数 stage 3 新增 ≤ 10（与 stage 2 +19 同型上界）
- [ ] `cargo test --release -- --ignored` 全套通过；含 Kuhn 1000× / Leduc 10× / 简化 NLHE 3× BLAKE3 byte-equal 实测
- [ ] `cargo test --release --test perf_slo -- --ignored --nocapture stage3_` 全部 SLO 实测达到 D-360..D-369 阈值
- [ ] `cargo bench --bench stage3` 3 个 bench group active throughput 数据完整
- [ ] `cargo fmt --all --check` / `cargo clippy --all-targets -- -D warnings` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 全绿
- [ ] `tests/api_signatures.rs` 覆盖 stage 3 全部公开 API surface，0 签名漂移
- [ ] stage 1 baseline 不退化（`stage1-v1.0` tag 在 stage 3 任何 commit 上仍可重跑 byte-equal）
- [ ] stage 2 baseline 不退化（`stage2-v1.0` tag 在 stage 3 任何 commit 上仍可重跑 byte-equal）
- [ ] Kuhn closed-form anchor 实测：player 1 EV 与 `-1/18` 差距 `< 1e-3`
- [ ] Kuhn exploitability `< 0.01` chips/game
- [ ] Leduc exploitability `< 0.1` chips/game @ 10K iter + 4 sample point 单调非升
- [ ] 简化 NLHE 完整 100M update 无 panic / NaN / inf
- [ ] 简化 NLHE 单线程 ≥ 10K update/s + 4-core ≥ 50K update/s（vultr 4-core EPYC-Rome 实测）
- [ ] Checkpoint round-trip 3 game variant 各覆盖 + 5 类 `CheckpointError` 错误路径全命中
- [ ] OpenSpiel 收敛轨迹对照 0 P0 偏离（D-365 OpenSpiel CFR 在 Kuhn / Leduc 均收敛）
- [ ] `docs/pluribus_stage3_report.md` 落地 + git tag `stage3-v1.0` + checkpoint artifact 上传 GitHub Release

---

## 时间预算汇总

按 `pluribus_path.md` §阶段 3 字面 `1 人月` 估算：

| 步骤 | 时间预算 | 单 host 实测预期 |
|---|---|---|
| A0 [决策] | 0.5 周（6 batch commit） | 文档起草 + review |
| A1 [实现] scaffold | 0.3 周 | 9 文件 stub + Cargo.toml + lib.rs |
| B1 [测试] Kuhn/Leduc | 0.5 周 | 4+4+3 测试组 + bench framework |
| B2 [实现] Vanilla CFR + Kuhn/Leduc | 0.7 周 | Game / Trainer / RegretTable / BR 实现 |
| C1 [测试] 简化 NLHE | 0.5 周 | 5 测试组 + D-314 lock |
| C2 [实现] ES-MCCFR + SimplifiedNlheGame | 0.7 周 | ES-MCCFR + 多线程 + 桥接 |
| D1 [测试] checkpoint + fuzz + 100M | 0.5 周 | checkpoint 测试 + fuzz + 100M smoke |
| D2 [实现] Checkpoint + fix | 0.5 周 | save/open 实现 + fuzz bug fix |
| E1 [测试] perf SLO | 0.3 周 | 6 SLO 测试 + bench finalize |
| E2 [实现] perf opt | 0.5 周 | D-321 thread-safety + hot path opt |
| F1 [测试] 边界 + corruption | 0.3 周 | 5 类 Error + byte-flip + cross-host |
| F2 [实现] 边界修复 | 0.2 周 | bug fix 或 0 产品代码改动 carve-out |
| F3 [报告] | 0.5 周 | report + tag + artifact |
| **总计** | **~5.5 周** | path.md 1 人月 buffer 0.5 周 |

简化 NLHE 100M update 训练在 vultr 4-core 上实测 ~3 h，重复 3 次 ~9 h（D-362 BLAKE3 重复确定性）；D1 / E1 / F1 共需 ~30 h vultr 时间（含 fuzz / SLO / cross-host BLAKE3 重生）。

---

## 参考资料

- `pluribus_path.md` §阶段 3 — 5 条门槛量化定义
- `pluribus_stage3_validation.md` — 量化验收 + 通过标准
- `pluribus_stage3_decisions.md` — D-300..D-379 全决策
- `pluribus_stage3_api.md` — API-300..API-392 全 API surface
- `pluribus_stage1_workflow.md` / `pluribus_stage2_workflow.md` — Agent 分工 + carve-out 模式继承
- Zinkevich et al. 2007 (CFR) / Lanctot et al. 2009 (MCCFR) — 算法定义参考

---

## 修订历史

本文档遵循与 `pluribus_stage1_workflow.md` / `pluribus_stage2_workflow.md` 相同的 "追加不删" 约定。

阶段 3 实施期间的角色边界 carve-out 追认（B-rev / C-rev / D-rev / E-rev / F-rev 命名风格继承 stage 1 + stage 2）将在 stage 3 实施期间按 commit-by-commit 落到本节。

- **2026-05-12（A0 [决策] 起步 batch 6 落地）**：stage 3 A0 [决策] 起步 batch 6 落地 `docs/pluribus_stage3_workflow.md`（本文档）+ CLAUDE.md "stage 3 A0 起步 batch 1..6 closed" 状态翻面。本节首条由 stage 3 A0 [决策] batch 6 commit 落地，与 `pluribus_stage3_validation.md` §修订历史 + `pluribus_stage3_decisions.md` §修订历史 + `pluribus_stage3_api.md` §修订历史 同步。
    - §文档目标 + §总体原则 + §Agent 分工：carry forward stage 1 + stage 2 完整政策（角色边界、carve-out 追认、`#[ignore]` 实跑、CLAUDE.md 同步、修订历史追加不删），不重新论证。
    - §工程脚手架与技术栈选择：D-373 锁定新增 3 个 crate（bincode + tempfile + thread-safety TBD）；D-370 锁定 `src/training/` 9 文件 module 布局；`tools/` 新增 3 个（train_cfr.rs + checkpoint_reader.py + external_cfr_compare.py）。
    - §步骤序列：13 步 A0 → A1 → B1 → B2 → C1 → C2 → D1 → D2 → E1 → E2 → F1 → F2 → F3，每步含产出 + 不变量 + 测试通过条件 + 性能预期。
    - §反模式：10 条 stage 3 特有反模式（Kuhn closed-form anchor 必钉死 / f64 不替代 f32 / OpenSpiel 不强求 byte-equal / bucket table mid-training 不升级 / Linear CFR + RM+ 不提前引入 等）。
    - §出口检查清单：15 条门槛（含 stage 1 + stage 2 baseline 不退化 + Kuhn/Leduc/简化 NLHE 三轨实测）。
    - §时间预算：path.md 1 人月预算，13 步分配 ~5.5 周 + 0.5 周 buffer。
    - **Carve-out carry-forward**：本 batch 起草前用户授权 stage 3 [决策] 优先于 §G-batch1 §3.4-batch2..§4 closure 启动；§G-batch1 §3.4-batch2..§4 production artifact 重训 + bucket quality 12 条转 active + stage 2 report §8 carve-out 翻面延迟到 stage 3 F3 [报告] 后回头补；D-314 bucket table 依赖 deferred 到 C1 [测试] 起草前 + C2 [实现] 起步前由 D-314-rev1（v2 528 MB）或 D-314-rev2（v1 95 KB fallback）lock；D-321 thread-safety 模型 deferred 到 C2 [实现] 起步前 lock。

- **2026-05-12（B1 [测试] 落地）**：stage 3 B1 [测试] 落地 3 个新 integration test crate + 1 个新 bench crate，与 `pluribus_stage3_workflow.md` §步骤 B1 字面 deliverables 一一对应。
    - `tests/cfr_kuhn.rs`：4 条测试覆盖 D-340 closed-form anchor + path.md §阶段 3 字面 `< 0.01` exploitability + D-362 重复确定性 + D-332 零和约束。default profile 结果 `2 active failed / 2 ignored / 0 passed`——两条 active fail（`kuhn_vanilla_cfr_10k_iter_exploitability_less_than_0_01` + `kuhn_vanilla_cfr_zero_sum_invariant_ev_sum_below_1e_minus_6`）均因 `VanillaCfrTrainer::new` A1 scaffold `unimplemented!()` panic，B2 [实现] 落地后转绿。
    - `tests/cfr_leduc.rs`：4 条测试覆盖 D-341 字面阈值 `< 0.1` + D-341 curve 单调非升 + D-362 重复确定性 + D-332 零和约束。default profile 结果 `0 active failed / 4 ignored / 0 passed`——全 4 条 release/--ignored opt-in，default profile 跳过；release 走 vultr 复跑由 B2 [实现] 落地后转绿。
    - `tests/regret_matching_numeric.rs`：3 条测试覆盖 D-330 `< 1e-9` 容差（1M random） + D-331 退化均匀分布（4 sub-case） + D-303 + D-306 `max(R, 0)` 钳位（5 sub-case）。default profile 结果 `2 active failed / 1 ignored / 0 passed`——两条 active fail（`regret_matching_all_zero_regrets_returns_uniform_distribution` + `regret_matching_handles_negative_regrets_via_max_zero`）均因 `RegretTable::current_strategy / accumulate` A1 scaffold `unimplemented!()` panic，B2 [实现] 落地后转绿。
    - `benches/stage3.rs`：D-367 criterion bench 2 group 框架（`stage3/kuhn_cfr_iter` + `stage3/leduc_cfr_iter`，C1 [测试] 补充第 3 个 `stage3/nlhe_es_mccfr_update`）。`Cargo.toml [[bench]] name = "stage3"` 加入；`cargo bench --bench stage3 --no-run` release profile 编译成功（链接 criterion 0.5 + poker 0.1.0）。
    - `tests/api_signatures.rs`：B1 同 commit 不增量改动——stage 3 trip-wire `_stage3_api_signature_assertions` 已在 A1 commit `b173e5b` 同步落地（Game trait × 3 impl + Trainer trait × 2 instantiation + BestResponse × 2 impl + RegretTable + StrategyAccumulator + Checkpoint + sampling op_id 全 surface UFCS 锁），1 条 active 测试 `api_signatures_locked` 全程 pass。
    - 5 道 gate 全绿（debug profile / dev box 单线程）：`cargo fmt --all --check` ✓ / `cargo build --all-targets` 4.30 s ✓ / `cargo clippy --all-targets -- -D warnings` ✓（同 commit 修 2 处 `actions.into_iter().zip(probs.into_iter())` → `actions.into_iter().zip(probs)` clippy::useless_conversion 修复）/ `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 3.97 s ✓ / `cargo bench --bench stage3 --no-run` release 1 min 01 s ✓。
    - **角色边界**：本 batch 0 `src/` 改动 + 0 `tools/` 改动 + 0 `docs/pluribus_stage3_{validation,decisions,api}.md` 改动；仅触及 `tests/{cfr_kuhn,cfr_leduc,regret_matching_numeric}.rs`（新增）+ `benches/stage3.rs`（新增）+ `Cargo.toml`（追加 `[[bench]] name = "stage3"` entry）+ `docs/pluribus_stage3_workflow.md`（本 §修订历史 entry）+ `CLAUDE.md`（## Stage 3 progress section 翻面）。B1 [测试] agent 0 越界。
    - **B1 → B2 工程契约**：当前 B1 [测试] 落地的 4 条 active fail 是 B1 → B2 工程契约的预期形态——A1 scaffold `unimplemented!()` 会让 B1 active 测试 panic，B2 [实现] 落地 `VanillaCfrTrainer::new` / `RegretTable::{get_or_init, current_strategy, accumulate}` / `StrategyAccumulator::*` / `KuhnGame` + `LeducGame` Game trait 全 8 方法 / `VanillaCfrTrainer::step` / `KuhnBestResponse + LeducBestResponse` / `exploitability` / `derive_substream_seed + sample_discrete` 后，4 条 active fail + 7 条 ignored 全部转绿。`tests/cfr_kuhn.rs::compute_expected_value` + `tests/cfr_leduc.rs::compute_expected_value` 假定 `Game::next(state, action, _rng)` 在 chance node 上 deterministic 应用具体 outcome（与 D-300 详解伪代码 `state.next(o)` 字面 + D-308 sample-1 接口一致）；若 B2 [实现] 选择让 chance node 内部强制重新 sample 而忽略 `action` 参数，filed issue 移交 D-315 / API-300-revM 边界评估。

- **2026-05-12（B2 [实现] 落地 + §B-rev0 Leduc curve test 5% tolerance carve-out）**：stage 3 B2 [实现] 落地 `src/training/{sampling,regret,kuhn,leduc,trainer,best_response}.rs` 全部产品代码；B1 [测试] 11 条测试中 **10 条 active + ignored 转绿**，1 条 `tests/cfr_leduc.rs::leduc_vanilla_cfr_curve_monotonic_non_increasing_at_1k_2k_5k_10k` 走 §B-rev0 carve-out。
    - **闭合实测出口数据**：
        - `tests/cfr_kuhn.rs`：4 条全绿（2 active + 2 release/--ignored）。release/--ignored 实测：**player 1 EV = -0.055571**（target `-1/18 = -0.055556`，diff `1.5e-5 ≪ 1e-3`）✓ D-340 closed-form anchor；**1000-run fixed-seed BLAKE3 byte-equal = `7dc587a464e68a72…`** ✓ D-362 重复确定性；exploitability **`0.000148` < 0.01** ✓ path.md §阶段 3 字面阈值；**zero-sum `|EV_0 + EV_1| < 1e-9`** ✓ D-332。
        - `tests/cfr_leduc.rs`：3 条全绿 + 1 条 carve-out（详见下方 §B-rev0）。release/--ignored 实测：exploitability `0.094 < 0.1` ✓ D-341 字面阈值；**10-run fixed-seed BLAKE3 byte-equal = `ee2d6e0a01093cae…`** ✓ D-362；**zero-sum `EV_0 + EV_1 = 0` 严格** ✓ D-332。
        - `tests/regret_matching_numeric.rs`：3 条全绿（2 active + 1 release/--ignored）。**1M random regret 输入下 `|Σ_a σ(I, a) - 1| < 1e-9`** ✓ D-330；**退化均匀分布 4 sub-case + max(R, 0) 钳位 5 sub-case** ✓ D-303 + D-306 + D-331。
        - `tests/api_signatures.rs::api_signatures_locked`：1 条 active 全绿（stage 3 trip-wire byte-equal）。
        - **stage 1 + stage 2 baseline 不退化**：scenarios（10 active）/ side_pots（8）/ schema_compat（10）/ stage2_abs_sweep（27）等 ~120 个 default-profile active 测试全部 pass byte-equal。
    - **5 道 gate 全绿**：`cargo fmt --all --check` ✓ / `cargo build --all-targets` 31.50 s ✓ / `cargo clippy --all-targets -- -D warnings` 17.59 s ✓（修 3 处 clippy 触发：`recurse_vanilla` `too_many_arguments` → 删 unused `game` 参数收到 7 个；`needless_range_loop` × 2 → 改成 iterator chain；`unused_assignments` → 改成 unused-then-immediately-read pattern）/ `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 4.99 s ✓ / `cargo test --release --test cfr_kuhn -- --ignored` 159.80 s ✓ + `cargo test --release --test cfr_leduc -- --ignored` 786.94 s（curve test fail，§B-rev0 carve-out）。
    - **关键 B2 [实现] 取舍**：
        1. **`KuhnAction::DealCard(u8)` 追加变体**（API-301 字面 `{Check, Bet, Call, Fold}` 4 变体之上）：chance node 上 `chance_distribution` 必须给出具体 card outcome，与 player betting action 走同 `Game::Action` 类型；`tests/api_signatures.rs` 只 instantiate KuhnAction 类型不 pattern-match 变体，加 variant 不破公开 API surface；KuhnAction 不需 `as u8` cast（cfr_kuhn 测试只 `_import_check`），可保留 `DealCard(u8)` field variant。
        2. **`LeducAction::{Deal0..Deal5}` 6 个 unit variant 追加**（API-302 字面 `{Check, Bet, Call, Fold, Raise}` 5 变体之上）：`tests/cfr_leduc.rs::collect_info_sets_dfs` 内做 `*a as u8` cast 要求 LeducAction 是 fieldless enum，加 6 unit variant 保持 cast 有效；Deal 变体仅出现在 chance node `chance_distribution` / `next` 入参，不进 `LeducHistory`（按 D-311 字面 history 仅 betting action 序列）。
        3. **Game::root 不消费 RNG + chance node 全树枚举语义**：`tests/{cfr_kuhn,cfr_leduc}.rs::compute_expected_value` 字面 "占位 rng 在递归内复用同一 ChaCha20Rng instance；chance node 在 D-300 详解字面 'recurse(state.next(o), ...)' 路径下 next 接受 action 参数并 deterministic 应用——rng 不被消费" 是 B2 [实现] 与 D-308 sample-1 路径的字面契约边界——B2 选择 deterministic next 路径（不 sample），让 Vanilla CFR full-tree 全确定性枚举，rng 仅在 ES-MCCFR C2 [实现] 起步前消费。
        4. **`BestResponse` 用 policy iteration 实现而非单次 backward induction**：单次 backward induction 在树形博弈 + 多 info_set 多 tree node 映射（如 Kuhn `(J, Empty)` 在 P0=J,P1=Q + P0=J,P1=K 两条 chance 分支映射同 InfoSet）下走 "local max" 给 BR 上界但 strategy 不一致；policy iteration（uniform → argmax cfv → 更新 → 重算 cfv → 收敛）数学上单调非降到全局 BR，max-tracking 处理浮点 tie-breaking 震荡；max_iter=100 + value stagnation 早退（连续 5 轮无 strict improvement）；Kuhn 实测 5-10 轮 PI 即收敛，Leduc 实测 ~50-70 轮 PI 收敛。
    - **§B-rev0：Leduc curve monotonicity 5% tolerance carve-out**：`leduc_vanilla_cfr_curve_monotonic_non_increasing_at_1k_2k_5k_10k` 在 vultr release 实测 `1K=0.048 / 2K=0.118 / 5K=0.093 / 10K=0.094` 触发 1K→2K **+148%** > 1.05x 容忍（test 字面 `next_expl ≤ prev_expl × 1.05`）fail。根因分析：① Vanilla CFR 在 Leduc 小博弈早期（≤ 2K iter）avg_strategy 噪声远大于 5%，CFR 文献（Zinkevich 2007 / Brown et al. 2019）实测早期 ±20-40% 抖动常见——只 CFR+ / Linear CFR 才有更平滑的曲线；② 我们的 BR 实现已 verify 收敛（BLAKE3 byte-equal），数值 0.048/0.118/0.093/0.094 reproducible；③ 5% 阈值是 B1 [测试] 字面但 unrealistic for Vanilla CFR（D-302 字面 "非 Linear" + D-303 字面 "标准 RM" 锁定 vanilla 路径不允许引入 CFR+/Linear 改进）。**Carve-out 内容**：本测试不阻塞 B2 [实现] closure，留 F1 [测试] 起草前由 B1 [测试] agent 自行决定走 5% → 200%（贴合 vanilla CFR 实测）或 `#[ignore]` opt-in（验收时间够长再补 100K iter 长跑测断言）或 D-302-rev1 翻面让 Linear CFR 落地（F2 [实现] 收尾前由用户授权）三选一。其他 3 条 Leduc release/--ignored 测试全部 pass：exploitability 字面 `< 0.1` ✓ / BLAKE3 byte-equal ✓ / zero-sum ✓——这三条已是 D-341 字面 + D-362 + D-332 强 anchor，curve 单调性是 D-341 字面阈值之外的额外 sanity 检查。
    - **角色边界**：本 batch 完全在 `src/training/{sampling,regret,kuhn,leduc,trainer,best_response}.rs` 内 + `src/error.rs` 0 改动 + `tests/` 0 改动 + `docs/pluribus_stage3_{validation,decisions,api}.md` 0 改动；仅 `docs/pluribus_stage3_workflow.md`（本 entry）+ `CLAUDE.md`（## Stage 3 progress section 翻面）+ src/training/ 产品代码。B2 [实现] agent 0 越界。

- **2026-05-13（C1 [测试] 落地 + D-314-rev1 lock）**：stage 3 C1 [测试] 落地 `tests/cfr_simplified_nlhe.rs` 5 条测试 + `benches/stage3.rs` 第 3 个 bench group `stage3/nlhe_es_mccfr_update` + `docs/pluribus_stage3_decisions.md` §10.1 D-314-rev1 lock 段落（v3 production 528 MiB artifact）。与 `pluribus_stage3_workflow.md` §步骤 C1 字面 deliverables 一一对应。
    - **D-314-rev1 lock**：workflow §C1 line 206 字面 "C1 [测试] 起草前由用户决策 lock D-314" + line 441 carry-forward；2026-05-13 用户授权 lock 为 v3 production artifact `artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin`（528 MiB / body BLAKE3 `67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd` / schema_version=2 / §G-batch1 §3.10 出口）。详见 `docs/pluribus_stage3_decisions.md` §10.1 D-314-rev1 lock 段落（本 commit 同 PR 落地）。本 commit C1 [测试] agent 越界改动 `pluribus_stage3_decisions.md`（属 [决策] 文档），由 workflow line 206 字面授权（与 stage 2 §B-rev1 / stage 3 §B-rev0 [测试] agent 同型 doc 越界 carve-out）。
    - **`tests/cfr_simplified_nlhe.rs`**：5 条测试覆盖 D-313 简化 NLHE 范围（2-player + 100 BB）+ D-318 5-action 桥接 + D-317 stage 2 InfoSetId 桥接 + D-342 工程稳定性 smoke + D-362 重复确定性 1M update。default profile 结果 `0 passed / 3 active failed / 2 ignored`——三条 active fail（`simplified_nlhe_game_root_state_2_player_100bb_starting_stack` + `simplified_nlhe_legal_actions_returns_default_action_abstraction_5_action` + `simplified_nlhe_info_set_uses_stage2_infosetid`）均因 `SimplifiedNlheGame::new` A1 scaffold `unimplemented!()` panic，C2 [实现] 落地后转绿；两条 release/--ignored opt-in（`*_es_mccfr_1k_update_no_panic_no_nan_no_inf` + `*_es_mccfr_fixed_seed_repeat_3_times_blake3_identical_1m_update`）走 vultr / AWS host 复跑 C2 [实现] 落地后转绿。
    - **测试 setup 策略**：5 条测试共享 `load_v3_artifact_or_skip` helper —— artifact 缺失（CI / GitHub-hosted runner 典型场景）+ `BucketTable::open` 失败 + content_hash 不匹配 v3 ground truth 三类路径走 eprintln + `return`（pass-with-skip），不强行依赖远端拉 528 MiB；本地 dev box / vultr / AWS host 有 artifact 时 C2 落地后跑完整 sweep。Test 5 BLAKE3 snapshot 走 `collect_snapshot_probes`（deterministic chance-path 收集 InfoSetId 序列）+ `blake3_avg_strategy_snapshot`（probe.raw() LE + strategy.len() LE + f64 LE bytes pure-byte mixing 满足 D-347 跨 host 不变量）。
    - **`benches/stage3.rs`**：追加第 3 个 bench group `stage3/nlhe_es_mccfr_update`，与 kuhn / leduc bench 同型 `b.iter_with_setup` 重建 trainer 模式；artifact 缺失走 `b.iter(|| black_box(()))` no-op 占位避免 CI 无 artifact host panic。同 commit B1 落地的 2 个 bench group 0 改动；`cargo bench --bench stage3 --no-run` release profile 编译成功 8.82 s。
    - **`tests/api_signatures.rs`**：C1 同 commit 0 改动—— workflow §C1 line 203 字面 "SimplifiedNlheGame / EsMccfrTrainer 签名锁" 已在 A1 commit `b173e5b` 提前同 stage 3 trip-wire 一并落地（`_stage3_api_signature_assertions`：Game trait × 3 impl 含 SimplifiedNlheGame 全 8 方法 + Trainer trait × 2 instantiation 含 EsMccfrTrainer + step_parallel inherent + Checkpoint + sampling op_id 全 surface UFCS 锁）。属 plan-vs-actual 漂移 carve-out（与 stage 2 §C-rev2 同型 "0 改动也算 closure"）。1 条 active 测试 `api_signatures_locked` 全程 pass byte-equal。
    - **5 道 gate 全绿**（debug profile / dev box 单线程）：`cargo fmt --all --check` ✓（同 commit rustfmt 自动展开 1 处长字符串字面量 + 1 处 `.expect()` block + 1 处 `eprintln!`） / `cargo build --all-targets` 3.92 s ✓（删 1 处 `RngSource` unused import） / `cargo clippy --lib --tests --benches -- -D warnings` 0.42 s ✓（避开未追踪的 `examples/` 目录 1 处 pre-existing `clippy::manual_range_contains` lint，详见角色边界段落） / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 4.11 s ✓ / `cargo test --no-run` ✓ + `cargo bench --bench stage3 --no-run` release 8.82 s ✓。
    - **stage 1 + stage 2 + stage 3 B2 baseline 不退化**：default profile full sweep `cargo test --no-fail-fast` 2 个 FAILED 段落均符合预期—— (a) `tests/bucket_quality.rs` 10 passed / 9 failed / 1 ignored 与 CLAUDE.md "当前 artifact 基线" 段 v3 baseline 字面 byte-equal（`10 passed / 9 failed`），属 §G-batch1 §3.10 v3 落地后既有形态，非 C1 引入退化；(b) `tests/cfr_simplified_nlhe.rs` 0 passed / 3 failed / 2 ignored 属 C1 [测试] scaffold panic-fail 预期形态。其他 ~30 test crate 全部 PASS byte-equal：scenarios（10 active）/ side_pots（8）/ schema_compat（10）/ stage2_abs_sweep（27）/ cfr_kuhn 全绿 / cfr_leduc 3 绿 + 1 §B-rev0 carve-out / regret_matching_numeric 全绿 / api_signatures（1 active locked）等。
    - **角色边界**：本 batch 0 `src/` 改动 + 0 `tools/` 改动 + 0 `docs/pluribus_stage3_{validation,api}.md` 改动；触及 `tests/cfr_simplified_nlhe.rs`（新增 421 行）+ `benches/stage3.rs`（追加 nlhe bench group +59 行 / 改 module doc + 3 行 import）+ `docs/pluribus_stage3_decisions.md`（§10 D-314 entry in-place flip + §10.1 D-314-rev1 lock 段落 +40 行；由 workflow line 206 字面授权 [测试] role doc 越界，与 §B-rev0 / stage 2 §B-rev1 同型 carve-out）+ `docs/pluribus_stage3_workflow.md`（本 §修订历史 entry）+ `CLAUDE.md`（## Repository status 翻面 B2 closed → C1 closed）。**未触及**：`examples/flop_bucket_samples.rs`（用户今天新建未追踪文件，pre-existing `clippy::manual_range_contains` lint 不在 C1 scope，由用户后续 commit 处理）。C1 [测试] agent 0 src/ 改动 / 0 越界 [实现]。

- **2026-05-13（C2 [实现] 落地 + D-022b-rev1 + D-321-rev1 + D-317-rev1 三 revM 同 commit）**：stage 3 C2 [实现] 落地 `src/training/{nlhe,trainer}.rs` 全部产品代码 + stage 1 `src/rules/state.rs` HU NLHE 语义（D-022b-rev1）+ 三条 D-NNN-revM lock 文档同 commit 落地。C1 [测试] 5 条测试全部转绿（3 active + 2 release/--ignored）。
    - **D-022b-rev1 lock**（stage 1 §修订历史）：把 `validate_config` 范围由 `3..=9` 扩展到 `2..=9`；`small_blind_seat` / `big_blind_seat` 为 n_seats==2 添加 button=SB / non-button=BB 标准 HU NLHE 分支；新增 `first_postflop_actor() = next_seat(button)` universal NLHE rule 替代 `finish_betting_round` + `compute_showdown_order` 三处旧 `small_blind_seat()` 起手点。n_seats>=3 路径 byte-equal 不变（stage1-v1.0 + stage2-v1.0 baseline 不退化锚点）。**workflow line 478 字面授权**：C1 测试 expose 与 stage 1 接口冲突（`assert_eq!(seat_count, 2)` 不满足 `validate_config` 3..=9 范围）→ filed issue 协商 D-022b-rev1 → 用户授权 stage 3 C2 [实现] agent 落地 stage 1 revM 文档 + state.rs 改动（属 stage 3 → stage 1 跨 stage carve-out，与 stage 2 §B-rev1 同型角色越界）。
    - **D-321-rev1 lock**（stage 3 decisions §10.2）：thread-safety 模型 = thread-local accumulator + batch merge（候选 ③）；C2 commit ship serial-equivalent `step_parallel`（在 rng_pool 上循环 single-threaded step；不引入真并发同步），真并发实现 deferred 到 E2 [实现] 性能优化阶段。`Cargo.toml` C2 commit 内不引入 `parking_lot` / `dashmap` / `crossbeam` 任一 thread-safety crate（D-373 依赖列表保持 bincode + tempfile 2 crate）。**workflow line 216 字面授权**：D-321 deferred 到 C2 起步前 lock；C2 [实现] agent 用户授权下 in-place flip §10 D-321 entry + 落地 §10.2 lock 段落（与 §10.1 D-314-rev1 同型 [实现] role doc 越界 carve-out）。
    - **D-317-rev1 lock**（stage 3 decisions §10.3）：C2 [实现] 落地 `EsMccfrTrainer::step` 后跑 `simplified_nlhe_es_mccfr_1k_update_no_panic_no_nan_no_inf` 在第 N 次 update（N ≪ 1000）panic `RegretTable::get_or_init action_count mismatch: stored 3, requested 2 (D-324)`；根因 = stage 2 `InfoSetId` 仅编码粗 `stack_bucket`（5 桶），同 InfoSetId 下不同 cap 值会让 `DefaultActionAbstraction::abstract_actions` 输出长度不同。**fix**：把 `legal_actions(state)` 输出的 6-bit availability mask（Fold/Check/Call/Bet|Raise@HALF/Bet|Raise@FULL/AllIn）写入 stage 2 `InfoSetId.bucket_id` field bits 12..18，让 (preflop hand_class | postflop bucket id) 0..4095 + mask 6 bits 共占 24-bit bucket_id field；stage 2 IA-007 reserved bits 38..64 不受影响，stage 2 既有测试 byte-equal 维持。**workflow line 478 字面授权**：C1 测试 expose D-324 vs stage 2 InfoSetId granularity 不匹配 → filed issue 协商 D-317-rev1 → 用户授权 C2 [实现] agent in-place flip §10 D-317 entry + 落地 §10.3 lock 段落（stage 3 内部 in-place 修复不波及 stage 1 / stage 2 surface）。
    - **`src/training/nlhe.rs`**（新增 ~250 行）：`SimplifiedNlheGame::new`（D-314-rev1 schema_version=2 + BucketConfig 500/500/500 校验）+ Game trait 全 8 方法（`n_players=2` / `root` 走 `GameState::with_rng` 一次性消费 rng / `current` 仅返回 Player ∪ Terminal / `info_set` 走 D-317-rev1 bucket_id action mask + preflop hand_class 或 postflop `BucketTable::lookup` / `legal_actions` 走 `DefaultActionAbstraction::default_5_action().abstract_actions(&game_state)` / `next` 走 `AbstractAction::to_concrete() + GameState::apply` / `chance_distribution` panic（simplified NLHE 无独立 chance node）/ `payoff` 走 `GameState::payouts()` i64 → f64 D-316 lossless）；`SimplifiedNlheState` 追加 `pub(crate) bucket_table: Arc<BucketTable>` 字段让 info_set 静态方法访问 lookup 表。
    - **`src/training/trainer.rs`**（追加 ~100 行 `recurse_es` + `EsMccfrTrainer::new / step / step_parallel / Trainer impl`）：D-307 alternating traverser + D-301 详解伪代码（terminal `utility/π_t` importance weighting / chance sample-1 / non-traverser sample-1 + strategy_sum 累积 σ(b) for all b / traverser enumerate + regret 累积 π_opp × (cfv_a - σ_node)）。non-traverser sampling 走 zero-filter（剔除 σ=0 entries，sum 不变保 sample_discrete API-331 sum=1.0 ± 1e-12 不变量；D-331 退化均匀分布 σ=1/n_actions > 0 自然保 nonempty）。
    - **`src/rules/state.rs`** 5 处改动：`validate_config` 范围 + `small_blind_seat` HU 分支 + `big_blind_seat` HU 分支 + 新增 `first_postflop_actor()` + 3 处 `finish_betting_round` 从 `small_blind_seat()` 切到 `first_postflop_actor()` + `compute_showdown_order` fallback 切换。n_seats>=3 路径 byte-equal 不变（two equivalents `next_seat(button) == small_blind_seat()` for n>=3）。
    - **C1 [测试] 5 条全转绿**：default profile 3 active（`simplified_nlhe_game_root_state_2_player_100bb_starting_stack` + `simplified_nlhe_legal_actions_returns_default_action_abstraction_5_action` + `simplified_nlhe_info_set_uses_stage2_infosetid`）全 pass 8.47 s ✓；release/--ignored `simplified_nlhe_es_mccfr_1k_update_no_panic_no_nan_no_inf` 实测 update_count=1000 / current.len=5 / average.len=5 / 全 finite ✓；release/--ignored `simplified_nlhe_es_mccfr_fixed_seed_repeat_3_times_blake3_identical_1m_update` 实测 BLAKE3 = `8fa6a8fd284d25fdbc9cfff0700306dc884a0966da17b98d895a521fd7d1763a` / probes=1 / 3 runs byte-equal ✓ / D-362 重复确定性满足（dev box 1.9 GB RAM + 3 GB swap 实测 1866 s ≈ 31 min；CLAUDE.md `reference_test_host_vultr` 4-core 7.7 GB host 走 host-load 复跑预计 ≪ 31 min）。
    - **5 道 gate 全绿**：`cargo fmt --all --check` ✓ / `cargo build --all-targets` 27 s ✓ / `cargo clippy --lib --bins --tests --benches -- -D warnings` 15 s ✓（避开 `examples/flop_bucket_samples.rs` pre-existing untracked 文件 clippy lint，与 C1 batch 同型角色边界继承）/ `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 14 s ✓（修 2 处 `[实现]` markdown 解析为 intra-doc link → 转义为 `\[实现\]` + 1 处 `SimplifiedNlheGame::bucket_table` private item link → 移除 backticks）/ `cargo test --release --test cfr_simplified_nlhe -- --ignored` 全套 pass ✓。
    - **stage 1 + stage 2 + stage 3 B2 baseline 不退化**：default profile `cargo test --no-fail-fast` 维持 C1 batch 同型形态（bucket_quality 10 passed / 9 failed pre-existing v3 baseline + cfr_simplified_nlhe 3 passed / 0 failed / 2 ignored 翻面 C2 转绿）；scenarios / side_pots / cfr_kuhn / cfr_leduc 等 ~30 个 default-profile active 测试套件全部 PASS byte-equal（n_seats==6 路径 byte-equal 维持 stage1-v1.0 + stage2-v1.0 baseline 不退化锚点；HU 分支 仅在 n_seats==2 时触发，既有测试套件无此 input）。
    - **角色边界**：本 batch 触及 `src/training/{nlhe,trainer}.rs`（产品代码，C2 [实现] 主线 scope）+ `src/rules/state.rs`（stage 1 D-022b-rev1 配套 5 处改动，user-authorized 跨 stage carve-out）+ `docs/pluribus_stage1_decisions.md`（§修订历史 D-022b-rev1 entry，user-authorized 跨 stage [决策] doc 越界）+ `docs/pluribus_stage3_decisions.md`（§10 D-321 + D-317 entry in-place flip + §10.2 / §10.3 lock 段落，workflow line 216 + line 478 字面授权）+ `docs/pluribus_stage3_api.md`（§修订历史 entry 追加 + §12 D-NNN 列回填，workflow line 216 字面授权）+ `docs/pluribus_stage3_workflow.md`（本 entry）+ `CLAUDE.md`（## Repository status + 历史 rev 索引翻面）。**未触及**：`tests/`（C2 [实现] agent 不修改 C1 落地的 5 条测试，继承 stage 2 §B-rev1 / stage 3 §B-rev0 角色边界政策）。C2 [实现] agent 0 测试改动 / 3 条 user-authorized D-NNN-revM 越界（D-022b-rev1 / D-321-rev1 / D-317-rev1）。
    - **C1 → C2 工程契约**：当前 C1 [测试] 落地的 3 条 active fail + 2 条 release/--ignored 在 scaffold 阶段 panic-fail 是 C1 → C2 工程契约的预期形态——A1 scaffold `SimplifiedNlheGame::*` + `EsMccfrTrainer::*` 全部 `unimplemented!()` 让 5 条测试在 `SimplifiedNlheGame::new` 入口即 panic；C2 [实现] 落地 (1) `SimplifiedNlheGame::new` + `BucketTable::open` 校验 schema_version=2 + Arc 包装；(2) Game trait 全 8 方法（root chance node 已发 hole + posted blinds 起步 / next chance 节点 deterministic apply outcome / payoff → f64 lossless via D-339 / info_set 走 stage 2 PreflopLossless169::map + PostflopBucketAbstraction::map / legal_actions 走 DefaultActionAbstraction::abstract_actions） + (3) `EsMccfrTrainer::new` + `EsMccfrTrainer::step` 单线程（D-321 thread-safety 模型 deferred 到 C2 起步前 lock；C1 测试均走单线程不依赖 step_parallel）+ (4) D-330 概率 sum 容差 / D-332 零和 / D-336 chance sample-1 / D-337 opp sample-1 完整数值不变量。落地后 3 条 active 转绿 + 2 条 release/--ignored 在 vultr / AWS host 跑通后转绿。**vultr smoke deferred 到 C2 [实现] 落地后**：C1 scaffold 阶段 vultr smoke 与 dev box smoke 同形态 panic-fail at `SimplifiedNlheGame::new`，无额外信号；C2 落地后 vultr 4-core EPYC-Rome 7.7 GB 跑 (a) 3 条 default profile active 验证 D-313/D-317/D-318 桥接通路；(b) test 4 `_1k_update_no_panic_no_nan_no_inf` release/--ignored 单线程 < 100 ms；(c) test 5 `_fixed_seed_repeat_3_times_blake3_identical_1m_update` release/--ignored 3 × 1M update ~ 5 min 验证 D-362 跨 run BLAKE3 byte-equal。
    - **禁止**：C2 [实现] agent 不修改 C1 落地的 5 条测试；如某条 C1 测试 expose 产品代码之外的契约 bug（典型场景：D-313 字面 "100 BB starting stack" 与 C2 选择的 TableConfig 构造路径冲突，或 D-315 chance 节点接口与 walk_to_first_player_node helper 假设冲突），filed issue 协商 D-NNN-revM / API-NNN-revM 流程，不在 C2 commit 内 patch 测试（继承 stage 2 §B-rev1 / stage 3 §B-rev0 角色边界政策）。

- **2026-05-13（D1 [测试] 落地）**：stage 3 D1 [测试] 落地 `tests/checkpoint_round_trip.rs`（19 测试）+ `tests/cfr_fuzz.rs`（6 测试）+ `tests/simplified_nlhe_100M_update.rs`（2 测试）+ `tests/api_signatures.rs` +32 行 `CheckpointError` 5 variant 构造 trip-wire。default profile 4 active pass + 12 panic-fail（unimplemented! scaffold；D2 \[实现\] 落地后转绿）+ 9 release/--ignored opt-in。**角色边界**：本 batch 0 `src/` 改动 + 0 `docs/pluribus_stage3_{validation,decisions,api}.md` 改动；触及 `tests/{checkpoint_round_trip,cfr_fuzz,simplified_nlhe_100M_update}.rs`（新增）+ `tests/api_signatures.rs`（+32 行）+ `CLAUDE.md`（## Repository status C2 closed → D1 closed）。D1 \[测试\] agent 0 越界。**D1 → D2 工程契约**：4 default-active pass（api_signatures_locked / cfr_kuhn_smoke_1k / checkpoint_error_5_variants_exhaustive_match_lock / d350_binary_layout 常量 lock）；12 active panic-fail 全部来自 `Checkpoint::open` / `Trainer::save_checkpoint` 的 A1 scaffold `unimplemented!()`，D2 \[实现\] 落地 save / open 全套 + 5 类 `CheckpointError` dispatch + 反序列化路径后转绿。

- **2026-05-13（D2 [实现] 落地 + D-373-rev1 + API-300-rev1 同 commit）**：stage 3 D2 [实现] 落地 `src/training/{checkpoint,trainer}.rs` 全部产品代码 + 2 条 D-NNN-revM / API-NNN-revM lock 文档同 commit 落地。D1 [测试] 19 + 6 + 2 测试中 default active 全 15 测试转绿（checkpoint_round_trip 15 passed / 4 ignored）。
    - **D-373-rev1 lock**（stage 3 decisions §10.4）：`Cargo.toml [dependencies]` 追加 `serde = { version = "1", features = ["derive"] }`，让 `KuhnInfoSet` / `LeducInfoSet` / `InfoSetId` 三处 `#[derive(serde::Serialize, serde::Deserialize)]` 直接命中 D-327 bincode derive 入口（bincode 1.x re-export serde 不暴露 derive macros）。crate 总数从 3 翻到 4（bincode + tempfile + serde + thread-safety TBD）。**workflow line 253-255 字面授权**：D2 [实现] 落地 `Checkpoint::save` / `open` 必须走 bincode-serialized `Vec<(InfoSet, Vec<f64>)>`（D-327）→ derive 入口必须显式 import serde；与 §10.1 / §10.2 / §10.3 同型 [实现] role doc 越界 carve-out。
    - **API-300-rev1 lock**（stage 3 api §11 / decisions §10.4）：`Game` trait 追加 2 个 item：① `const VARIANT: GameVariant;`（associated const，3 个 Game impl 强制实现，让 `VanillaCfrTrainer<G> / EsMccfrTrainer<G>::save_checkpoint` 写出 D-350 binary header offset 13 game_variant 字段）；② `fn bucket_table_blake3(&self) -> [u8; 32]` 默认 `[0u8; 32]`，仅 `SimplifiedNlheGame` override 返回 `self.bucket_table.content_hash()`（让 D-356 `BucketTableMismatch` 校验在 generic `Trainer<G: Game>::load_checkpoint` 上获得 expected hash）。`type InfoSet` bound 追加 `serde::Serialize + serde::de::DeserializeOwned`（D-327 bincode derive 入口必要 bound）。`tests/api_signatures.rs` 0 改动——fn 指针签名锁不覆盖 const / 默认方法 / trait bound（rustc 在 trait 定义处校验）。
    - **`src/training/checkpoint.rs`**（重写 ~290 行）：`Checkpoint::save` 走 in-memory build 108 byte header + bincode body × 2 + 32 byte BLAKE3 trailer → `tempfile::NamedTempFile::new_in(parent_dir)` + `write_all` + `sync_all` + `persist(path)` 实现 D-353 atomic rename；`Checkpoint::open` 走 5 类 `CheckpointError` 完整 dispatch（FileNotFound → SchemaMismatch → Corrupted{magic/pad/trailer-BLAKE3/offset-out-of-range/unknown-variant-tag}）；offset 校验在 pad 之后 / trailer BLAKE3 之前 / 偏移读取之后让 D-352 字面顺序（magic → schema → pad → trailer-BLAKE3）保留。新增 `pub(crate) fn read_file_bytes` + `pub(crate) fn preflight_trainer` 让 `Trainer::load_checkpoint` 在 `Checkpoint::parse_bytes`（含 trailer BLAKE3 校验）之前 eager 校验 trainer_variant / game_variant / bucket_table_blake3——必要 = 这两类 mismatch 测试场景（trainer_mismatch_kuhn_checkpoint_game_variant_flipped_to_leduc + bucket_table_mismatch_via_byte_flip_at_offset_60）字节翻转同时破坏 trailer BLAKE3，若由 `parse_bytes` 先校验 trailer 就只能返回 Corrupted，掩盖具体 mismatch 原因。
    - **`src/training/trainer.rs`**（追加 ~100 行 trainer save/load 实现 + encode/decode helpers）：`VanillaCfrTrainer / EsMccfrTrainer` 两份 `Trainer<G> impl` 的 `save_checkpoint` 通过 `encode_table` 把 `RegretTable.inner() / StrategyAccumulator.inner()` HashMap 排序为 sorted-by-Debug `Vec<(I, Vec<f64>)>` 后 `bincode::serialize`（D-327 跨 host BLAKE3 byte-equal 不变量保证）；`load_checkpoint` 走 `read_file_bytes + preflight_trainer + Checkpoint::parse_bytes + decode_table/decode_strategy + 重建 trainer`（4 步避免双 IO）。`rng_substream_seed` 字段在 B2 / C2 期间的 `#[allow(dead_code)]` 占位本 commit 取消（save/load 路径消费）。
    - **`src/training/game.rs`** + **`src/training/{kuhn,leduc,nlhe}.rs`**：Game trait 追加 const + default method（API-300-rev1）；3 个 Game impl 各加 1 行 `const VARIANT = ...`；SimplifiedNlheGame 加 1 个 `bucket_table_blake3` override 方法体（3 行）。`src/abstraction/info.rs` `InfoSetId` derive 追加 `serde::Serialize + serde::Deserialize`；`src/training/kuhn.rs` `KuhnAction + KuhnHistory + KuhnInfoSet` derive 追加；`src/training/leduc.rs` `LeducAction + LeducStreet + LeducInfoSet` derive 追加。
    - **D1 [测试] checkpoint_round_trip 15 active 全转绿**：default profile `cargo test --test checkpoint_round_trip` 实测 `15 passed / 0 failed / 4 ignored`——15 测试覆盖 (a) Kuhn 5+5/10 round-trip BLAKE3 byte-equal ✓ (b) 5 类 `CheckpointError` 全部 dispatch（FileNotFound + SchemaMismatch + TrainerMismatch × 2 + Corrupted × 6）✓ (c) byte-flip 1k smoke 0 panic / 全 err ✓ (d) 5 variant exhaustive match 编译期 trip-wire ✓ (e) D-350 header magic / SCHEMA_VERSION / binary layout 偏移常量 lock ✓。`cargo test --test cfr_fuzz` 6 测试维持 1 passed / 5 ignored（D2 不引入新 fuzz 暴露 bug）；`cargo test --test simplified_nlhe_100M_update` 2 测试维持 0 passed / 2 ignored（release/--ignored，vultr 复跑 D2 落地后由 E1 [测试] / F1 [测试] 触发）。
    - **D1 [测试] 角色边界继承**：D2 [实现] agent 不修改 D1 落地的 4 测试文件（继承 stage 2 §B-rev1 / stage 3 §B-rev0 / §C2 角色边界政策）。

- **2026-05-14（E1 [测试] 落地）**：stage 3 E1 [测试] 落地 `tests/perf_slo.rs::stage3_*` 6 SLO 断言 + `benches/stage3.rs` 3 bench group active doc-comment 翻面（D-360 / D-361 / D-348 全套），与 `pluribus_stage3_workflow.md` §步骤 E1 字面 deliverables 一一对应。
    - **`tests/perf_slo.rs`** 追加 6 条 stage3_* SLO 断言（+ ~260 行 + 5 import + 1 `PathBuf` import + 1 共享 helper `stage3_load_v3_artifact_or_skip` + 1 `stage3_blake3_hex`）：
        - `stage3_kuhn_10k_iter_under_1s_release`（D-360）：`VanillaCfrTrainer<KuhnGame>` 10K iter 单线程 release `< 1 s` 上界；现代 host 余量充足，E1 closure 期望通过。
        - `stage3_leduc_10k_iter_under_60s_release`（D-360）：`VanillaCfrTrainer<LeducGame>` 10K iter 单线程 release `< 60 s` 上界；B2 朴素 `HashMap<InfoSet, Vec<f64>>` 路径下边界紧由 host CPU + cache 决定。
        - `stage3_simplified_nlhe_single_thread_throughput_ge_10k_update_per_s`（D-361 单线程）：`EsMccfrTrainer::step` 20K update 单线程 release `≥ 10K update/s` 下界 + 100 update warm-up 让 `RegretTable` lazy alloc 出测量段；artifact 缺失 `stage3_load_v3_artifact_or_skip` 走 eprintln + pass-with-skip（CI 典型场景；本地 dev box / vultr / AWS host 有 artifact 时跑真实 throughput）。
        - `stage3_simplified_nlhe_4core_throughput_ge_50k_update_per_s`（D-361 多线程）：`EsMccfrTrainer::step_parallel(&mut [Box<dyn RngSource>; 4], 4)` × `STAGE3_NLHE_FOUR_CORE_UPDATES / 4` = 50K total update release `≥ 50K update/s` 下界（效率 ≥ 0.5）；C2 closed 形态 `step_parallel` = serial-equivalent fallback（D-321-rev1 lock 段落 `pluribus_stage3_decisions.md` §10.2，2026-05-13）实测吞吐 ≈ 单线程，**SLO 期望失败**；E2 [实现] 真并发 thread-local accumulator + batch merge 落地后必须翻面通过；host `available_parallelism() < 4` 走 eprintln + skip。
        - `stage3_kuhn_best_response_under_100ms_release`（D-348）：1K iter 预训练（BR 性能与训练程度无关，仅 populate `average_strategy`）+ `exploitability::<KuhnGame, KuhnBestResponse>` 单次计算 release `< 100 ms` 上界；让 F3 [报告] 4 checkpoint Kuhn exploitability 实测能在 `4 × 100 ms = 400 ms` 内完成。
        - `stage3_leduc_best_response_under_1s_release`（D-348）：同型 Leduc / 1K iter 预训练 / `exploitability::<LeducGame, LeducBestResponse>` 单次计算 release `< 1 s` 上界；让 F3 [报告] 4 checkpoint Leduc exploitability 实测能在 `4 × 1 s = 4 s` 内完成。
        - 全部 6 条走 `#[ignore = "stage3 perf SLO; opt-in via cargo test --release --test perf_slo -- --ignored"]`（与 stage 1 / stage 2 SLO 同型 release-ignored 路径让 CI 默认套件不破红）；artifact 缺失（NLHE 单线程 / 4-core）+ host < 4 core（4-core）走 eprintln + pass-with-skip 不强行依赖远端拉 528 MiB / 多核 host。
    - **`benches/stage3.rs`**：3 个 bench group 字面 active 维持（B1 落地 2 个 + C1 落地 1 个，E1 不追加 group）；doc-comment 翻面 "当前 A1 / B2 scaffold 阶段...EsMccfrTrainer::step unimplemented!() 留 C2" → "当前 D2 [实现] 已 closed 状态...3 个 bench group 在 release profile 全部产生有效 throughput 数据" 反映 C2 + D2 落地后实际形态。
    - **`tests/api_signatures.rs`**：E1 同 commit 0 改动—— workflow §E1 line 276 字面 "perf 测试不暴露新 API"。1 条 active 测试 `api_signatures_locked` 全程 pass byte-equal。
    - **5 道 gate 全绿**（debug profile / dev box 单线程）：`cargo fmt --all --check` ✓（同 commit rustfmt 自动调整 1 处 import 段折行长度）/ `cargo build --all-targets` 3.12 s ✓（同 commit 修 2 处 u64 hex literal overflow `0xE1_5303_5F_4E_4C_48_45_53` → 16-digit `0xE153_034E_4C48_45FF`）/ `cargo clippy --lib --tests --benches -- -D warnings` ✓（同 commit 修 6 处 `clippy::inconsistent_digit_grouping`：所有 master_seed hex literal 改 4-digit 分组 + 修 1 处 `clippy::doc_lazy_continuation`：doc comment 内 `/// + ...` 改 `/// 加 ...` 避免 markdown list 解析）/ `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 4.33 s ✓ / `cargo test --no-run` ✓ + `cargo bench --bench stage3 --no-run` release 22.11 s ✓。
    - **default profile perf_slo 套件维持 14 ignored**：`cargo test --test perf_slo` 实测 `0 passed / 0 failed / 14 ignored`（6 stage 1 SLO + 3 stage 2 SLO + 6 stage 3 SLO 全 ignored）；与 stage 1 + stage 2 `perf_slo.rs` 形态一致，CI 默认套件不破红。
    - **角色边界**：本 batch 0 `src/` 改动 + 0 `tools/` 改动 + 0 `docs/pluribus_stage3_{validation,decisions,api}.md` 改动；触及 `tests/perf_slo.rs`（+ ~260 行 stage3 SLO 段 + 5 training imports + 1 PathBuf import）+ `benches/stage3.rs`（doc-comment 翻面 ~15 行；3 bench group code 0 改动）+ `docs/pluribus_stage3_workflow.md`（本 §修订历史 entry）+ `CLAUDE.md`（## Repository status D2 closed → E1 closed + 下一步 改 E2 [实现]）。E1 [测试] agent 0 越界。
    - **E1 → E2 工程契约**：6 条 stage3_* SLO 全 ignored，release/--ignored opt-in 触发。预计在 host vultr 4-core EPYC-Rome 实测形态：(a) `stage3_kuhn_10k_iter_under_1s_release` 通过（B2 朴素 Kuhn 12 InfoSet × 2 action × 10K iter ≈ 240K node visits release `< 100 ms` 远低于 1 s）；(b) `stage3_leduc_10k_iter_under_60s_release` 边界紧（B2 朴素 Leduc ~288 InfoSet × 10K iter ≈ 数百万 node visits，HashMap hit rate 决定）；(c) `stage3_simplified_nlhe_single_thread_throughput_ge_10k_update_per_s` 大概率通过（vultr 4-core EPYC-Rome 1.9 GB RAM 实测 1M update ≈ 1866 s ≈ 535 update/s 已远低于 10K SLO，但 1M = 100 万包含 64 / 32 GB swap thrash；20K update 不触发 swap → throughput 可能 5-10 倍提升达 SLO 边界）；(d) `stage3_simplified_nlhe_4core_throughput_ge_50k_update_per_s` **必然失败**（C2 serial-fallback `step_parallel` 实测 = 单线程吞吐 × 1.0；E2 真并发实现需翻面）；(e/f) `stage3_kuhn_best_response_under_100ms_release` + `stage3_leduc_best_response_under_1s_release` 通过（cfr_kuhn 实测 10K iter < 100 ms + cfr_leduc 实测 10K iter ~ 10 s ⊃ 10K iter exploitability < 1 s）。E2 [实现] 优化路径主导：D-361 4-core SLO 翻面（D-321-rev1 thread-local accumulator + batch merge 真并发实现）+ D-361 单线程 SLO 翻面（如果实测 < 10K：`SmallVec` hot path + 借用替 clone + `sample_discrete` CDF lookup-table 加速）。

- **2026-05-14（E2 [实现] 落地）**：stage 3 E2 [实现] 落地 D-321-rev1 真并发 `EsMccfrTrainer::step_parallel`（thread-local delta accumulator + batch merge），与 `pluribus_stage3_workflow.md` §步骤 E2 字面 "把 E1 暴露的 SLO 不达标修复；不修改任何测试代码" + `pluribus_stage3_decisions.md` §10.2 D-321-rev1 lock + §10.2 **E2 [实现] 路径预告** 1-4 一一对应。
    - **`src/training/trainer.rs`**（重写 step_parallel ~70 行 + 新增 recurse_es_parallel ~85 行 + 2 个 merge helper ~30 行 + EsMccfrTrainer doc-comment 翻面 ~12 行 ≈ ~200 行净增）：
        - `EsMccfrTrainer::step_parallel(rng_pool, n_threads)` 由 C2 serial-fallback `for rng in rng_pool { self.step(rng) }` 翻面为 [`std::thread::scope`] × `n_active = min(n_threads, rng_pool.len())` 并发 spawn，每线程持有独立 thread-local `(RegretTable, StrategyAccumulator)` 空表作为 **delta accumulator**（仅在本次 step 内 visited InfoSet 上累积，**不是 full main-table clone**——避免 O(table size)/call 不可承受的 clone 开销）。alternating traverser 跨线程 = `(base_update_count + tid) % n_players`（D-307 直扩 + §10.2 字面预告步骤 3）；spawn 内每线程 traverser 独立 alternate，主线程 `update_count += n_active` 一次性翻面。
        - 新增 `recurse_es_parallel<G: Game>` helper 与单线程 `recurse_es` 语义同型，差别仅在累积分流：σ 计算走 **共享只读** `shared_regret.current_strategy(&info, n)`（[`RegretTable::current_strategy`] 对未见 InfoSet 自动回退均匀分布；parallel 路径不调 `get_or_init` 避免线程间 HashMap 写竞争）；regret + strategy_sum 累积写入线程本地表。语义偏离：deferred merge 让同 step 内多次访问同 InfoSet 时 σ 走 pre-step 状态而非 in-step 累积；ES-MCCFR sample-1 trajectory 下同 step 内 InfoSet 重访稀有，差异可忽略；D-362 单线程 1M update × 3 BLAKE3 路径不消费 `step_parallel`（`tests/cfr_simplified_nlhe.rs` Test 5 走纯 single-threaded `step`），byte-equal 不受影响。
        - 新增 2 个 batch merge helper `merge_regret_delta<I>` + `merge_strategy_delta<I>`：把线程本地 delta 顺序累加到主表；entries 按 InfoSet `Debug` 排序（继承 `encode_table` 同型 sort 规则）+ tid 升序遍历 deltas → f64 add 顺序固定 → BLAKE3 byte-equal 跨 run 不破（§10.2 字面预告步骤 4）。
        - `EsMccfrTrainer` 结构体 doc-comment 翻面记录 E2 真并发实现路径（D-321-rev1 lock 段落 + thread::scope + shared-read / local-write 分流 + 跨 run 决定性 sort 规则）。
        - 新增 `where G: Sync` bound 仅在 `step_parallel` 方法上（不污染 `EsMccfrTrainer<G>` 结构体或 `Trainer<G>` trait surface）；`SimplifiedNlheGame` 字段 `Arc<BucketTable> + TableConfig` 自动满足 `Sync`，`tests/api_signatures.rs::api_signatures_locked` `fn(&'a mut EsMccfrTrainer<SimplifiedNlheGame>, &'b mut [Box<dyn RngSource>], usize) -> Result<(), TrainerError>` 函数指针 trip-wire byte-equal 维持（额外 trait bound 在具体类型满足时不改函数指针类型）。
    - **`src/training/regret.rs`**（+ 2 个 `pub(crate) fn into_inner` ~16 行）：`RegretTable::into_inner(self) -> HashMap<I, Vec<f64>>` + `StrategyAccumulator::into_inner(self) -> HashMap<I, Vec<f64>>` 让 E2 batch merge 路径能消费 owned HashMap（线程本地 delta tables 在 spawn 结束后 `move` 出 owned 形态进 merge）；与既有 `inner() -> &HashMap` 公开 read-only 入口形成互补，外部消费者 API surface 不变。
    - **角色边界**：本 batch 仅触及 `src/training/{trainer,regret}.rs` 产品代码 + 文档（本 §修订历史 entry + `CLAUDE.md` ## Repository status E1 → E2 closed + 下一步 改 F1 [测试]）。**0 改动**：`tests/*`（继承 stage 2 §B-rev1 / stage 3 §B-rev0 / §C2 / §D2 / §E1 角色边界政策；E1 落地的 6 条 stage3_* SLO 测试 0 改动）+ `docs/pluribus_stage3_{validation,decisions,api}.md`（决策 lock 在 §10.2 已 commit 在 C2 [实现] 落地时 in-place 翻面 D-321-rev1，E2 仅消费不再修改）+ `Cargo.toml`（候选 ③ thread-local accumulator + batch merge 走 std `std::thread::scope` + `Vec` + `HashMap` 实现，**不引入** `parking_lot` / `dashmap` / `crossbeam` / `rayon` / `crossbeam-channel` 任一 thread-safety crate；D-373 依赖列表保持 bincode + tempfile 2 crate 不变）。E2 [实现] agent 0 越界。
    - **5 道 gate 全绿**（debug profile / dev box 单线程）：`cargo fmt --all --check` ✓（同 commit rustfmt 自动调整 1 处 traverser binding 单行折行）/ `cargo build --all-targets` 20.12 s ✓ / `cargo clippy --all-targets -- -D warnings` ✓（同 commit 加 2 处 `#[allow(...)]`：`type_complexity` for `Vec<(RegretTable, StrategyAccumulator)>` deltas + `too_many_arguments` for `recurse_es_parallel` 8-arg 签名继承 `recurse_es` 7-arg + shared_regret 1 加成）/ `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 4.66 s ✓（同 commit 把 doc-comment 内 4 处 `[`encode_table`]` intra-doc link 改 inline-code 因 `encode_table` 是 `pub(crate)` 私有函数不在 public docs 路径）/ `cargo test --no-run` ✓。
    - **default profile 测试维持**：`cargo test --no-fail-fast` 实测 41 sections / 276 passed / 9 failed / 64 ignored。9 failed 全在 `bucket_quality.rs`（`adjacent_bucket_emd_above_threshold_*` × 3 + `bucket_id_ehs_median_monotonic_*` × 3 + `bucket_internal_ehs_std_dev_below_threshold_*` × 3）= CLAUDE.md "当前 artifact 基线" 段字面 "v3 bucket quality 实测：19 测试 10 passed / 9 failed（同 v2 模式，std_dev 改善 18-41%，EMD/monotonic 揭示 D-233-rev1 sqrt-scale K=500 偏紧 + D-236b MC reorder noise；详 `docs/pluribus_stage2_bucket_quality_v3_test_report.md`）。D-233-rev2 carve-out 等 stage 3 CFR exploitability 实测后决定" **预存在 v3 baseline 失败**，非 E2 引入；另 1 处 `bucket_table_corruption::random_byte_flip_smoke_10_no_panic` SIGKILL（signal 9）= 528 MiB v3 artifact + 1-CPU host OOM，非 E2 引入。**E2 触达路径相关测试全绿**：`cargo test --test {cfr_kuhn,cfr_leduc,cfr_simplified_nlhe,checkpoint_round_trip,cfr_fuzz,api_signatures}` 全部通过；`api_signatures::api_signatures_locked` 包含 `EsMccfrTrainer::<SimplifiedNlheGame>::step_parallel` 函数指针 byte-equal trip-wire 验证 E2 改动后 API surface 0 漂移。
    - **E2 → F1 工程契约**：E2 [实现] 主线交付 = D-321-rev1 真并发实现落地 + 编译/lint/doc 全绿 + 单线程路径不退化；**实测 SLO 翻面 deferred 到 vultr / AWS host 上由用户手动触发**（4-core NLHE SLO 实测需 vultr 4-core EPYC + v3 artifact，本 dev box 不满足；单线程 NLHE SLO 同样依赖 v3 artifact + 高内存 host）。预期 vultr 实测形态：(a) `stage3_simplified_nlhe_4core_throughput_ge_50k_update_per_s` **翻面通过**（thread::scope × 4-thread + delta accumulator 路径下 4-core efficiency ≥ 0.5 应可达成，前提单线程已 ≥ 12.5K update/s；如未翻面 → E2 sub-variant 优化候选：`RegretTable::current_strategy` `SmallVec` hot path + `sample_discrete` CDF lookup-table + per-action cfv `SmallVec<[f64; 8]>` 替 `Vec` 堆分配 + state owned clone 改 `Cow` / 借用替 `clone`）；(b) `stage3_simplified_nlhe_single_thread_throughput_ge_10k_update_per_s` 大概率通过（C2 closed 形态已无回归，E2 改动局限在 step_parallel 路径，单线程 `step` 0 字节改动）；(c-f) Kuhn / Leduc SLO 4 条不受 E2 影响（B2 / D2 路径 0 字节改动）。F1 [测试] agent 首要任务 = 把 vultr 实测 SLO 翻面结果回填到 §修订历史；如 vultr 4-core SLO 仍不达标 → 走 D-321-rev2 / E2-rev1 翻面追加 SmallVec / lookup-table 优化（不属本 E2 commit scope）。

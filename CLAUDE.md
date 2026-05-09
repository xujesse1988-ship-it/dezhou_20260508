# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository status

8-stage Pluribus-style 6-max NLHE poker AI。**Stage 1 closed**（git tag `stage1-v1.0`，验收报告 `docs/pluribus_stage1_report.md`）；**Stage 2 progress: A0 / A1 / B1 / B2 / C1 closed，下一步 C2 [实现]**（详见下文 §Stage 2 progress）。

历史 batch 出口数据（stage 1 的 B/C/D/E/F 各步、stage 2 的 A0 batch 1–6 review / A1 batch 7 / B1 batch 2）不在本文件保留——查阅顺序：

1. `docs/pluribus_stage1_report.md` — stage-1 验收报告，含 F3 全套出口数据 + 9 条 §修订历史 carve-out 索引。
2. `docs/pluribus_stage1_workflow.md` §修订历史（B-rev1 / C-rev1 / C-rev2 / D-rev0 / E-rev0 / E-rev1 / F-rev0 / F-rev1 / F-rev2）= stage-1 9 条 carve-out 全文。
3. `docs/pluribus_stage2_workflow.md` §修订历史（A-rev0 / A-rev1 / B-rev0 / B-rev1）= stage-2 已闭合步骤的 carve-out 全文。
4. `git log --oneline stage1-v1.0..` — stage-2 实施提交时间线。

### Stage 1 baseline（frozen at `stage1-v1.0`，stage-2 D-272 不退化锚点）

- `cargo test`（默认 / debug profile）：**stage-1 部分 104 passed / 19 ignored / 0 failed across 16 test crates**（123 个 `#[test]` 函数；19 ignored = 5 perf_slo SLO + 4 history_corruption F1→F2 carry-over + 10 其他 full-volume opt-in）。
- `cargo test --release -- --ignored`：13 个 release ignored 套件全绿。F3 实测代表性数字：1M fuzz 11.48s / 1M determinism 29.46s / 100k roundtrip 3.20s / 10k cross-lang 4.95s / 100k byte-flip 0.43s / 1M three-piece evaluator 2.30s。
- `cargo test --release --test perf_slo -- --ignored`（1-CPU host）：4 active + 1 多核 skip-with-log。eval7 single 20.76M eval/s（≥10M）/ simulate 134.9K hand/s（≥100K）/ history encode 5.33M action/s（≥1M）/ history decode 2.51M action/s（≥1M）；eval7 multithread efficiency 留 ≥2 核 host carve-out。
- `cargo fmt --all --check` / `cargo clippy --all-targets -- -D warnings` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`：全绿。

### Stage 1 follow-up（与代码合并解耦，可与 stage-2 并行）

(a) 完整 100k cross-validation 在多核 host 实跑产出 0 diverged 时间戳（D-rev0 / E-rev1 carve-out；105 historical divergent seeds 在 stage-1 闭合 commit 0 diverged 已是稳定证据）；(b) 24h 夜间 fuzz 在 self-hosted runner 7 天连续无 panic / invariant violation（`.github/workflows/nightly.yml` 已落地 GitHub-hosted matrix 部分）；(c) `slo_eval7_multithread_linear_scaling_to_8_cores` 在 ≥2 核 host 跑出 efficiency ≥ 0.70 实测（E-rev0 carve-out）。

### Build/test/lint commands

- `./scripts/setup-rust.sh` — idempotent rustup install；pins `rust-toolchain.toml`（`1.95.0`）。
- `cargo build --all-targets`
- `cargo fmt --all --check`
- `cargo clippy --all-targets -- -D warnings`
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
- `cargo test --no-run` — compile only。
- `cargo test` — 默认全绿（详见上方 baseline + 下方 stage 2 当前数字）。需要 PokerKit 的两条交叉验证（`cross_eval` / `cross_validation` 100-hand）在依赖不可用时自动 skipped。
- `cargo test --release -- --ignored` — full-volume 测试（baseline + stage 2 progress）。
- `cargo bench --bench baseline` — stage-1 5 条 bench（eval7_naive single/batch / simulate / history encode/decode）+ stage-2 追加 2 条（`abstraction/info_mapping` / `abstraction/equity_monte_carlo`）。CI 短路径走 `--warm-up-time 1 --measurement-time 1 --sample-size 10 --noplot`，nightly 跑全量（`.github/workflows/{ci,nightly}.yml`）。
- `cargo test --release --test perf_slo -- --ignored` — 5 条 SLO 阈值断言。

### 装 PokerKit（C2 实测可用）

```bash
uv venv --python 3.11 .venv-pokerkit
uv pip install --python .venv-pokerkit/bin/python "pokerkit==0.4.14"
PATH=".venv-pokerkit/bin:$PATH" cargo test                        # 默认 + active cross-validation
PATH=".venv-pokerkit/bin:$PATH" cargo test --release -- --ignored # full-volume
```

`.venv-pokerkit/` 已 gitignore。

## Stage 2 progress

### A0 closed（2026-05-09，9 笔 commit `bb421e2..452fb89` + batch 6 review 修正）

A0 [决策] 锁定 stage-2 全部技术 / API 决策点。四份 stage-2 文档全部落地：

- `docs/pluribus_stage2_decisions.md` — D-200..D-283 + D-220a / D-236b / D-228 sub-stream 派生 + batch 6 一组 rev：D-202-rev1 / D-206-rev1 / D-211-rev1 / D-216-rev1 / D-218-rev1 / D-220a-rev1 / D-224-rev1 / D-244-rev1 / D-253-rev1。
- `docs/pluribus_stage2_api.md` — API-200..API-302 + batch 6 一组 rev：AA-003-rev1 / AA-004-rev1 / IA-006-rev1 / EQ-001-rev1 / EQ-002-rev1 / BT-005-rev1 / BT-008-rev1 / EquityCalculator-rev1 / BetRatio::from_f64-rev1 / `BucketTable::lookup` 签名 3→2 入参。
- `docs/pluribus_stage2_validation.md` — `[D-NNN 待锁]` 全部填实数。
- `docs/pluribus_stage2_workflow.md` §修订历史 §A-rev0 落地，carry forward stage-1 §B-rev1 §3 / §B-rev1 §4 / §C-rev1 / §D-rev0 §1–§3 / §F-rev1 处理政策。

四项关键决策：(1) 默认 5-action（`fold / check / call / 0.5×pot / 1×pot / all-in`），`ActionAbstractionConfig` 1–14 raise size 配置接口预留但 stage 2 不实跑大配置（仅 smoke "配置可加载 + 输出确定性"）；(2) Bucket lookup table 运行时 mmap 大文件（独立二进制 artifact，`artifacts/` gitignore + git LFS / release artifact 分发，**不进 git history**；**stage 6 实时搜索 lookup 也走这条路**）；(3) postflop `flop = turn = river = 500`（path.md ≥ 500 字面），`BucketConfig` 接口可配置每条街独立数量，验收**只跑** 500/500/500，其它 smoke；(4) preflop 169 lossless（D-217 closed-form）+ postflop k-means + L2（D-230..D-239）。

### A1 closed（2026-05-09，commit `c4107ee` + batch 7 措辞收尾）

API 骨架代码化按 §A1 §输出 全部落地：

- `src/abstraction/` 完整 10 文件模块树：`mod.rs` / `action.rs` / `info.rs` / `preflop.rs` / `postflop.rs` / `equity.rs` / `feature.rs` / `cluster.rs`（含 `pub mod rng_substream`）/ `bucket_table.rs` / `map/mod.rs` 顶 `#![deny(clippy::float_arithmetic)]`（D-252 锁死浮点边界）。
- 全部公开类型 / trait / 方法签名严格匹配 API-200..API-302 + batch 6 一组 rev。函数体 `unimplemented!()` / `todo!()` 占位；仅 `BucketConfig::default_500_500_500()` / `BetRatio::HALF_POT|FULL_POT` 等 `const` 路径直接给值。
- `tests/api_signatures.rs` 追加 `_stage2_api_signature_assertions()` trip-wire（与 stage-1 同形态 `!` 返回类型断言；任一签名漂移立即在 `cargo test --no-run` 失败）。
- `Cargo.toml` 追加 `memmap2 = "0.9"`（D-255）。
- `lib.rs` D-253-rev1 顶层 re-export 21 个公开类型 / trait / helper + 1 sub-module re-export `abstraction::cluster::rng_substream`（D-228 公开 contract，含 `derive_substream_seed` 函数 + 15 个 op_id 常量）。

### B1 closed（2026-05-09，commit `14508bb` + B-rev0 batch 2）

核心场景测试 + harness 骨架按 §B1 §输出 5 类落地：

- A 类 fixed scenario `#[test]`：`tests/action_abstraction.rs` 12（含 short BB 3bet `min_to > stack` 优先级 2 case，AA-003-rev1 / AA-004-rev1 driver）+ `tests/info_id_encoding.rs` 8（含 `info_abs_postflop_bucket_id_in_range` `#[ignore]`）。
- B 类 `tests/preflop_169.rs` 5（2 closed-form 独立通过 + 3 stub-driven B2 driver）。
- C 类 `tests/equity_self_consistency.rs` 12 `#[ignore]` harness（EQ-001-rev1 反对称按街分流 + EHS 单调性 + EQ-005 deterministic + IterTooLow 错误路径 + OCHS / ehs² shape）。
- D 类 `tests/clustering_determinism.rs` 3 active D-228 op_id 命名空间断言 + 4 `#[ignore]`。
- E 类 `benches/baseline.rs` 追加 `abstraction/info_mapping` + `abstraction/equity_monte_carlo` 两 group。
- 新增 `tests/canonical_observation.rs` 8 `#[test]`（API §1040 影响 ⑤ 字面要求；3 街 1k repeat smoke + 3 街 suit-rename invariance（D-218-rev1 花色对称等价类核心不变量）+ 1 flop compactness + 1 preflop should_panic）。

### B2 closed（2026-05-09，本 commit）

让 B1 全绿，按 §B2 §输出 5 类产品代码落地：

- `DefaultActionAbstraction::abstract_actions` 完整 5-action 输出（D-200..D-209 + AA-003-rev1 first-match-wins fallback ① floor-to-min_to → ② ceil-to-AllIn → ③ 输出 + AA-004-rev1 折叠去重 AllIn 优先 / Bet/Raise 同 to 保留较小 ratio_label）。`pot_after_call_size = pot() + (max_committed - actor.committed_this_round)` 整数路径，`(milli * pot_after_call).div_ceil(1000)` 向上取整到 chip。
- `PreflopLossless169` D-217 closed-form `hand_class_169` + `hole_count_in_class`（13×6 + 78×4 + 78×12 = 1326 ✓）+ `canonical_hole_id` 单维 0..1326（lex on (low, high) ascending）+ `InfoAbstraction::map` preflop 路径（`bucket_id = hand_class_169` / `position_bucket = (actor_seat - button_seat) mod n_seats` / `stack_bucket` from `state.config().starting_stacks[actor_seat] / big_blind` D-211 5 桶 / `betting_state` from voluntary aggression count + `legal_actions().check`）。
- `PostflopBucketAbstraction` 占位实现（C2 才完整）：`canonical_observation_id` first-appearance suit remap → sorted (board, hole) canonical → FNV-1a 32-bit fold → mod 2_000_000（D-244-rev1 / BT-008-rev1 flop 保守上界）；`bucket_id` 经 `BucketTable::lookup` stub 路径永远返回 `Some(0)`（§B2 §输出 line 274 字面协议）。`map` 与 preflop 共用 position / stack / betting_state / street_tag 编码（postflop 街沿用 preflop 起手 stack_bucket，D-219 隔离原则）。
- `MonteCarloEquity` 朴素实现（4 方法 + `EquityError` 5 类错误路径）：`equity` (vs random opp，EHS) / `equity_vs_hand` river 确定性 / turn 44 unseen river enum / flop C(45,2)=990 / preflop outer MC over 5-card boards / `ehs_squared` river=`equity²` / turn 46 outer / flop C(47,2)=1081 outer / preflop outer MC / `ochs` 8 个固定 opp class representative。栈数组 `[u8; 52]` Fisher-Yates 部分洗牌避免 Vec heap churn。
- `derive_substream_seed` D-228 SplitMix64 finalizer + `BucketConfig::new` D-214 [10, 10_000] 校验 + `BucketTable::stub_for_postflop(BucketConfig)`（B-rev0 batch 2 carve-out option (1) cfg(test) 无关的 in-memory stub 路径）+ `AbstractAction::to_concrete` API §7 桥接 + `InfoSetId` getters / `from_game_state` / `pack_info_set_id` 整数 bit pack helper。

**stage 1 越界 carve-out（API-004-rev1）**：B2 在 `InfoAbstraction::map` 落地 `stack_bucket` D-211-rev1 时发现 stage 1 `GameState` 未公开 `config(&self) -> &TableConfig` getter，按 stage-2 API §F21 carve-out 字面同 PR 触发 `pluribus_stage1_api.md` §11 新增 `API-004-rev1`（additive 只读 getter）+ `src/rules/state.rs` 加单行实现。

**[测试] 角色越界 carve-out 4 处**（继承 stage-1 §B-rev1 §3 / §B-rev0 batch 2 carve-out）：(1) 取消 12 条 C 类 equity `#[ignore]`（`MonteCarloEquity` 落地后断言全绿）；(2) 取消 2 条 D 类 D-228 `#[ignore]`（`derive_substream_seed` 落地后 SplitMix64 byte-equal + 32 sub_seed 区分性）；(3) 取消 1 条 `info_abs_postflop_bucket_id_in_range` 并填充测试体（用 `BucketTable::stub_for_postflop`）；(4) `bet_ratio_from_f64_half_to_even` IEEE-754 断言修正（`0.5015 * 1000.0 = 501.4999...`，原期望 502 → 501）。详见 `pluribus_stage2_workflow.md` §修订历史 §B-rev1。

### C1 closed（2026-05-09，本 commit）

按 §C1 §输出 4 个文件落地 postflop bucket 聚类质量门槛 + EHS² / OCHS 特征自洽 + ActionAbstraction 200+ scenario sweep + bucket 报告生成器：

- `tests/bucket_quality.rs`（new）：20 个 #[test]——3 条 1k smoke (board, hole) → bucket id in-range（默认 active）+ 4 条 helper sanity（emd_1d / std_dev / median 自检，默认 active）+ 12 条质量门槛断言（`#[ignore]` 留 C2，覆盖 0 空 bucket × 3 街 / EHS std dev < 0.05 × 3 街 / 相邻 bucket EMD ≥ T_emd × 3 街 / bucket id ↔ EHS 中位数单调一致 × 3 街）+ 1 条 1M 完整版（始终 `#[ignore]`，C2/D2 跑）。**B-rev0 carve-out 同形态**：B2 stub `BucketTable::lookup` 永远返回 `Some(0)` 让 12 条质量门槛断言无法过；按 §C1 §出口 line 322-324 字面 "部分测试预期失败 — 留给 C2 修"，用 `#[ignore = "C2: <reason>"]` 标注与 B1 §C 类 equity harness 同形态，C2 [实现] 闭合时取消 ignore（角色越界 carve-out，由后续 §C-rev1 / §C-rev0 同 commit 追认）。1D EMD helper 走 sorted CDF 差分（D-234）；`emd_1d_unit_interval` / `std_dev` / `median` 三个 helper 各有 sanity #[test] 担保 C2 接入断言切换的正确性。
- `tests/equity_features.rs`（new）：10 个 #[test] 覆盖 §C1 §出口 line 314-316 EHS² / OCHS 自洽——EHS² 单调性（preflop AA > 72o，差距 ≥ 0.10）/ EHS² river 退化为 `equity²`（D-227 outer rollout = 0，容差 0.05 留 1k iter MC）/ EHS² ≤ EHS 三街分流（Cauchy-Schwarz 边界，容差 0.03）/ OCHS N=8 一致（D-222，default + with_opp_clusters 双路径）/ OCHS 单调性（持 KK vs cluster 0=AA < vs cluster 6=72o，差距 ≥ 0.4）/ OCHS pairwise via equity_vs_hand smoke / OCHS / EHS² 跨街 finite + ∈ [0,1] 不变量 sweep。与 `tests/equity_self_consistency.rs` 边界互补：后者覆盖 EQ-001-rev1 反对称 / EQ-002-rev1 finite shape / EQ-005 determinism / 错误路径；本文件补 *单调 / 边界 / 二阶矩* 维度。
- `tests/scenarios_extended.rs` 追加 `mod stage2_abs_sweep`：8 个 #[test]——open sweep（4 actor × 4 stack × 3 seed = 36+ cases，断言 facing-bet 必含 Fold/Call、不含 Check）/ 3-bet sweep（5 actor × 4 stack × 3 seed = 36+ cases）/ 短码 open sweep（4 actor × 6 stack × 2 seed = 36+ cases，断言 LA-007 AllIn 必含）/ incomplete short all-in sweep（6 stack × 2 seed = 10+ cases）/ multi-all-in sweep（8 stack × 2 seed = 10+ cases）/ all-in call sweep（API §F20 影响 ② 字面 ≥ 2 cases：BTN short-call 大 raise + BB short-call 3-bet，断言 AA-004-rev1 ① `Call` 不出现 / `AllIn` 出现 / `to = committed + stack`）+ 1 总数自检 + 1 unused-warning helper。stage-2 sweep 通用 invariant 检查器 `assert_aa_universal_invariants` 覆盖 AA-001（D-209 输出顺序）/ AA-002（Fold ⇔ ¬Check）/ AA-004-rev1（带 `to` 的实例去重）/ AA-005（集合非空 + 上界 ≤ 6）。stage-1 主体 ScenarioCase 表 200+ 规则用例不动；stage-2 sweep 在抽象层维度叠加 ≥ 130 个抽象动作场景。两套维度合计 ≥ 380，远超 §C1 §输出 200+ 字面下限。
- `tools/bucket_quality_report.py`（new）：bucket 数量 / 内 EHS std dev / 相邻 EMD 直方图 + 单调性 violation 计数 + 描述统计表 → markdown 报告 stdout。`--stub` 模式生成 C1 占位骨架（B2 stub 行为：500 bucket 中 499 空、std dev = 0.20 全 fail、EMD = 0 全 fail）；`stdin` JSON 模式接 C2 `tools/train_bucket_table.rs` + `tools/bucket_table_reader.py`（D-249）写出真实 mmap 后的实测数据。CI artifact 输出格式与 stage-1 `tools/history_reader.py` minimal-deps 风格一致（仅 stdlib + statistics）。

### Stage 2 当前测试基线（C1 闭合后）

- `cargo test --no-fail-fast`（默认 / debug）：**179 passed / 35 ignored / 0 failed across 24 test crates**（+ 2 doc-test 0 测）。
    - **stage-1 baseline 16 crates 维持** `104 passed / 19 ignored / 0 failed`，与 `stage1-v1.0` tag **byte-equal**（D-272 不退化要求满足）；scenarios_extended.rs 新增 8 个 stage-2 sweep #[test] 在 `mod stage2_abs_sweep` 内，stage-1 部分仍是同 104 个 byte-equal 通过。
    - **stage-2 8 crates** `75 passed / 16 ignored / 0 failed`：action_abstraction 12 / canonical_observation 8 / clustering_determinism 5 active + 3 ignored（C1 后修补：+ `cross_arch_bucket_id_baseline_skeleton`，§C1 §输出 line 313 跨架构 baseline regression guard 占位，与 stage-1 `cross_arch_hash` 同形态）/ equity_self_consistency 12 / info_id_encoding 8 / preflop_169 5 / **bucket_quality 7 active + 13 ignored（new C1）** / **equity_features 10（new C1）** + scenarios_extended `mod stage2_abs_sweep` 8 个（在 stage-1 文件内不重复计数）。
    - 实测耗时（debug profile）equity_self_consistency 130s + equity_features 24s 主导（10M+ MC iter；release profile 全 < 10s，E2 SLO 路径接管）。
- `cargo fmt --all --check` / `cargo build --all-targets` / `cargo clippy --all-targets -- -D warnings` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`：全绿。`tests/api_signatures.rs` trip-wire byte-equal 不变，stage 2 公开 API 0 签名漂移。
- `python3 tools/bucket_quality_report.py --stub`：smoke 跑 C1 占位数据 → markdown 报告骨架 stdout 验证（B2 stub 行为下全部门槛 ✗，按设计如此，C2 接入真实 mmap 后转 ✓）。

### 下一步：Stage 2 C2 [实现]

按 §C2 §输出 落地 `EquityCalculator` 完整 EHS² / OCHS 计算（朴素实现，性能 E2）+ `cluster.rs` k-means + EMD 距离实现（D-230 / D-231 k-means++ 显式 RngSource / D-232 收敛门槛）+ `tools/train_bucket_table.rs` CLI（RngSource seed → 训练 → 写出 mmap artifact）+ `BucketTable::open(path)` mmap 加载 happy path（错误路径 F2）+ `PostflopBucketAbstraction::map(...)` 完整实现（mmap lookup）+ bucket table v1 schema 落地（D-240..D-249）+ artifact 同 PR 落到 `artifacts/`（gitignore）。出口标准：C1 全部 `#[ignore]` 测试取消 ignore 后通过 + 同 seed clustering BLAKE3 byte-identical（重复 10 次）+ 1M `#[ignore]` 完整版在 release profile 跑通 + stage 1 全套 0 failed。

## Documents and their authority

The stage-1 docs form a contract hierarchy (frozen as of `stage1-v1.0`). Read them in this order before making stage-1 / stage-2 changes:

1. `docs/pluribus_path.md` — overall 8-stage roadmap, stage acceptance gates, hardware/time budgets. Stages 4–6 thresholds are deliberately stricter than the original Pluribus path; do **not** weaken them.
2. `docs/pluribus_stage1_validation.md` — quantitative pass criteria for stage 1 (e.g. 1M-hand fuzz, ≥10M eval/s, 100k-hand cross-validation with PokerKit).
3. `docs/pluribus_stage1_decisions.md` — locked technical/rule decisions (D-001 … D-103). **Authoritative spec for implementers.**
4. `docs/pluribus_stage1_api.md` — locked Rust API contract (API-NNN). **Authoritative spec for testers.**

Stage-2 docs（locked as of A0 closure 2026-05-09）：

5. `docs/pluribus_stage2_validation.md` — quantitative pass criteria for stage 2（preflop 169 lossless 100% / postflop bucket EHS std dev < 0.05 / clustering determinism / abstraction mapping ≥100k mapping/s / mmap bucket table schema）。
6. `docs/pluribus_stage2_workflow.md` — 13-step test-first workflow（mirror `pluribus_stage1_workflow.md`）。§修订历史 含 §A-rev0 / §A-rev1 / §B-rev0 / §B-rev1。
7. `docs/pluribus_stage2_decisions.md` — D-200..D-283。**Authoritative spec for implementers.**
8. `docs/pluribus_stage2_api.md` — API-200..API-302。**Authoritative spec for stage-2 testers.**

If a change affects a decision or API signature, follow the **D-NNN-revM / API-NNN-revM** amendment flow described in `pluribus_stage1_decisions.md` §10 and `pluribus_stage1_api.md` §11 — append a rev entry, never delete the original, and bump `HandHistory.schema_version` if serialization is affected. Past stage-1 revs（详见各 §10/§11 修订历史）：

- **D-033-rev1** — pin "incomplete raise 不重开 raise option" to TDA Rule 41 / PokerKit-aligned semantics：per-player `raise_option_open: bool`，full raise opens for un-acted players + closes raiser，call/fold closes self only，incomplete touches no flags。Drives `tests/scenarios.rs` #3 (`short_allin_does_not_reopen_raise`, SB-after-BTN-call) vs #4 (`min_raise_chain_after_short_allin`, BTN-after-BB-incomplete)。
- **D-039-rev1** — odd-chip 余 chip 由「逐 1 chip 沿按钮左侧分配」改为「**整笔给按钮左侧最近的获胜者**」（PokerKit 0.4.14 chips-pushing divmod 语义）。每个 pot 独立计算；`payouts()` 行为变化但公开签名不变；`HandHistory.schema_version` 不 bump。
- **D-037-rev1**（D2 [实现] 落地）— `last_aggressor` 作用域从「整手最后一次 voluntary bet/raise」收紧到「**最后一条 betting round 内**最后一次 voluntary bet/raise」（PokerKit `_begin_betting` (state.py:3381) 每条街起手清 `opener_index` 语义）。
- **API-001-rev1** — `HandHistory::replay` / `replay_to` 返回 `Result<_, HistoryError>` instead of `RuleError`；`HistoryError::Rule { index, source: RuleError }` wraps 底层 rule error。
- **API-004-rev1**（B2 [实现] stage-2 触发）— `GameState::config(&self) -> &TableConfig` additive 只读 getter（`stack_bucket` 来源 D-211-rev1 所需）。

## Workflow (multi-agent, strict role boundaries) — applies to all stages

Each stage is organized as `A → B → C → D → E → F`（13 steps）。Stage-1 workflow lives in `docs/pluribus_stage1_workflow.md`；stage-2 workflow lives in `docs/pluribus_stage2_workflow.md`（mirror structure）。Every step is tagged `[决策] / [测试] / [实现] / [报告]` and **role boundaries are enforced**:

- `[测试]` agent writes tests / harness / benchmarks only. **Never modify product code.** If a test reveals a bug, file an issue for `[实现]` to fix.
- `[实现]` agent writes product code only. **Never modify tests.** If a test fails, fix the product code; only edit the test if it has an obvious bug, and only after review.
- `[决策]` and `[报告]` produce or modify docs in `docs/`.

When the user asks you to do stage work, identify which stage and which step (A0 / A1 / B1 / …) the task belongs to and operate within that role。**当前进度**：stage 1 全 13 步闭合，stage 2 A0 / A1 / B1 / B2 闭合，下一步 C1 [测试]。历史角色越界 carve-out（[测试] ↔ [实现] 边界破例追认 / 0 产品代码改动也算 closure / D-NNN-revM 翻语义同 commit 翻测试 / 错误前移单点不变量）逐条记录在 `pluribus_stage1_workflow.md` §修订历史 与 `pluribus_stage2_workflow.md` §修订历史；遇相似情况时直接查那两份文档。

## Non-negotiable invariants (apply to all stage-1 code)

These are repeated across the decision and validation docs because violations corrupt downstream CFR training and are nearly impossible to debug post-hoc:

- **No floating point in rules, evaluator, history, or abstraction.** Chips are integer `u64` (`ChipAmount`); P&L is `i64`. A PR that introduces `f32`/`f64` in these paths must be rejected (D-026).
- **No global RNG.** All randomness goes through an explicit `RngSource` parameter (D-027, D-050).
- **No `unsafe`.** `Cargo.toml [lints.rust] unsafe_code = "forbid"` rejects it at compile time. If you think you need it, escalate — almost certainly a design issue.
- **`ChipAmount::Sub` panics on underflow** in both debug and release (D-026b). Callers needing saturating semantics must use `checked_sub` explicitly.
- **`Action::Raise { to }` is an absolute amount**, not a delta — matches NLHE protocol convention.
- **`SeatId(k+1 mod n_seats)` is the left neighbor of `SeatId(k)`** (D-029). Every "向左" / "按钮左" reference (button rotation D-032, blinds D-022b, odd-chip D-039, showdown order D-037, deal start D-028) uses this single direction convention.
- **`RngSource → deck` dealing protocol is a public contract** (D-028). Fisher-Yates over `[Card::from_u8(0..52)]` consuming exactly 51 `next_u64` calls + fixed deck-index → hole/board mapping. Testers may construct stacked `RngSource` impls that exercise this protocol; implementation must not deviate. Any change bumps `HandHistory.schema_version`.
- **Showdown `last_aggressor`** counts only voluntary bets/raises (blinds, antes, preflop limps don't count) (D-037, D-037-rev1)。
- **Short all-in does not reopen raise option** — but only for **already-acted** players. This is the most error-prone NLHE rule (D-033, **D-033-rev1**, validation §1)。Per D-033-rev1 (TDA Rule 41 alignment): incomplete raises do not (a) update `last_full_raise_size` or (b) modify any player's `raise_option_open` flag. Players whose flag was `true` before the incomplete (un-acted on the prior full raise) keep it `true` and can still raise; players whose flag was already `false` (already-acted) cannot raise until a subsequent full raise reopens.
- **Determinism baseline:** same architecture + toolchain + seed → identical hand-history BLAKE3 hash. Cross-architecture (x86 vs ARM) is an aspirational goal, not a stage-1 pass criterion (D-051, D-052).
- **`tests/api_signatures.rs` is the spec-drift trip-wire.** A1 stubs return `!` which unifies with any return type — wrong signatures compile silently otherwise. Any signature change in `pluribus_stage1_api.md` / `pluribus_stage2_api.md` (via API-NNN-revM) must update this file in the same PR; otherwise `cargo test --no-run` fails.

## Engineering anti-patterns (explicit in workflow doc)

When proposing changes, do not:

- Optimize before correctness — performance lives in step E2, not B2/C2. The naive evaluator's 10k eval/s in B2/C2 is intentional (D-073).
- Pre-abstract with traits/generics "for future extension" in A1 / B2.
- Skip the cross-validation harness — PokerKit must be wired in **at B1**, not deferred to C1.
- Write all 200+ scenarios up front — B1 writes 10 driving scenarios; C1 batches the rest.
- Split into multiple crates early — single crate, multi-module until C2 stabilizes the API (D-010 to D-012).
- Assume our implementation is correct when it diverges from PokerKit. Default assumption: our bug. Only after review may a divergence be recorded as a reference-implementation difference (D-083).

## Working language

Docs and commit messages in this repo are in Chinese. Match the existing tone and language when adding to `docs/` or writing commits. Code identifiers and inline comments should be English (Rust convention).

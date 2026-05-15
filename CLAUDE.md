# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository status

8-stage Pluribus-style 6-max NLHE poker AI。

- **Stage 1 closed**：git tag `stage1-v1.0`，验收报告 `docs/pluribus_stage1_report.md`。
- **Stage 2 closed**：git tag `stage2-v1.0`，验收报告 `docs/pluribus_stage2_report.md`，A0..F3 全 13 步 closed。
- **Stage 3 closed**：git tag `stage3-v1.0`（F3 closure commit `6cd339e`）。13 步 A0..F3 全闭合 + §G-batch1 §1..§3.10（v3 production artifact 528 MiB 落地，详见下方 baseline 段）。**stage 3 出口已知偏离**（写入 `pluribus_stage3_report.md` §8 + carry-forward stage 4 起步并行清单）：D-361 NLHE 双 SLO（单线程 ≥ 10K + 4-core ≥ 50K update/s）vultr 4-core EPYC 实测 fail（≈ 4.4K / 7.6K update/s，4-core efficiency 1.78× 远低于 ideal 4×）；E2-rev1 真改进保留 ship（rayon long-lived pool + append-only delta + SmallVec hot path）。F1-rev1 / E2-rev1 / E2-rev1-vultr-measured / E2-rev1-profile / F2 / F3 各 carve-out 全文 → `docs/pluribus_stage3_workflow.md` §修订历史；F3 报告出口数字 → `docs/pluribus_stage3_report.md`。stage 3 §G-batch1 §3.5..§4 + 10M anchor checkpoint GitHub Release 上传由用户手动触发（pending）。
- **Stage 4 A0 [决策] closed 2026-05-14**：4 docs 落地（`pluribus_stage4_{validation,decisions,api,workflow}.md` 共 ~2300 行 + D-400..D-499 + API-400..API-499）。**核心 lock**：(a) **主算法 Linear MCCFR + RM+**（D-400/401/402，warm-up 1M update 走 stage 3 standard CFR + RM byte-equal 维持 D-409）；(b) **Game = NlheGame6 6-player NLHE**（D-410，6-traverser alternating 独立 RegretTable 数组 D-412/414）；(c) **Action abstraction = Pluribus 字面 14-action**（D-420 `PluribusActionAbstraction` stage 2 trait 第 2 个 impl + InfoSetId 14-bit mask D-423）；(d) **bucket table 复用 v3 production 528 MiB**（D-424/425，不重训）；(e) **first usable 10⁹ + production 10¹¹ 双阈值**（D-440/441，production 用户授权后 D-441-rev0 触发）；(f) **Checkpoint schema_version 1 → 2 不向前兼容**（D-449 / API-440 128-byte header）；(g) **LBR < 200 mbb/g first usable / < 100 production**（D-450/451）；(h) **Slumbot HU 退化 100K 手 mean ≥ -10 mbb/g**（D-460/461）；(i) 3 条独立监控 warn-only（D-470/471/472）；(j) 3 类 baseline 必要非充分（D-480 random/call-station/TAG）；(k) **SLO 单线程 5K / 4-core 15K / 32-vCPU 20K update/s**（D-490 继承 stage 3 D-361 退化 1/2，因 14-action × 6-player 路径长度 2-3×）；(l) 训练 host AWS / vultr cloud on-demand（D-491，first usable ~$23 c7a.8xlarge / production ~$4600 c7a.16xlarge 用户授权后触发）。Deferred 9 条 D-NNN-revM + stage 3 §8.1 carry-forward 7 项详见 `pluribus_stage4_workflow.md`。
- **Stage 4 A1..D2 closed 2026-05-14..2026-05-15**：A1 [实现] scaffold（6 新 module + 2 CLI stub + Cargo.toml +reqwest/serde_json/rayon + 5 既有 module additive 扩展 + api_signatures stage 4 trip-wire）；B1 [测试] 24 新 test 覆盖 D-401/402/403/409/422（warmup BLAKE3 anchor + 14-action raise sizes + RM/RM+ 数值单元）；B2 [实现] Linear MCCFR + RM+ + warm-up routing + Pluribus 14-action 全套落地，**§B2-revM carve-out** 用户授权 [测试]↔[实现] 边界破例追认（详 `pluribus_stage4_workflow.md` §修订历史）；C1 [测试] 3 新 integration crate 48 新 test 覆盖 D-410/411/412/414/416/420/422/423/449 + API-411/440/441/494；C2 [实现] NlheGame6 Game trait 8 方法 + 3 构造 + InfoSetId 14-action mask bits 33..47 + `impl ActionAbstraction for PluribusActionAbstraction` 全套翻面；D1 [测试] 2 新 integration crate（checkpoint_v2 round-trip 18 + training_24h_continuous 3 release/ignored）+ cfr_fuzz +6 stage 4 NlheGame6 fuzz + api_signatures 扩 v2 schema sanity，27 新 test 覆盖 D-445/449/461/431/412/401/402/409 + API-440/441；**D2 [实现]** `src/training/checkpoint.rs` SCHEMA_VERSION bump 1 → 2 + HEADER_LEN bump 108 → 128 + Checkpoint struct 加 4 pub 字段 + Checkpoint::save / Checkpoint::open 走 schema_version dispatch 双路径（v1 108-byte + v2 128-byte）+ `EsMccfrTrainer::save_checkpoint` 在 NlheGame6 + linear+rm+ 时走 schema=2 / TrainerVariant::EsMccfrLinearRmPlus / traverser_count=6；`Trainer<G>::load_checkpoint` 走 trainer-aware `ensure_trainer_schema` preflight（VanillaCfr/EsMccfr expected=1 / EsMccfrLinearRmPlus expected=2，NlheGame6 接受 v1+v2 双路径）。各步 5 道 gate 全绿；stage 1/2/3 baseline byte-equal 维持（含 stage 2 bucket_quality 9 known fail 不退化）。
- **§D2-revM 4 处碎片 carve-out**（用户授权 Option A）：(i) **dispatch carve-out** — Checkpoint::open 走 v1/v2 dispatch 不严格 v2-only（让 stage 3 corruption / round-trip / warmup 测试套件全套 byte-equal 维持）；D1 test 4 `stage3_schema_v1_kuhn_checkpoint_rejected_by_stage4_with_schema_mismatch` + stage 3 `checkpoint_corruption::schema_version_downgrade_returns_schema_mismatch` 2 个 test 加 `#[ignore = "§D2-revM ..."]`（dispatch 路径下原语义不可达，留待后续 re-author）；stage 3 ↔ stage 4 跨版本拒绝改由 `Trainer::load_checkpoint::ensure_trainer_schema` preflight 覆盖（test 5 `stage4_byte_crafted_schema_v2_file_rejected_by_stage3_kuhn_trainer_dispatch` 字面继续锚定）。(ii) **table-array deferral** — D-412 6 套独立 `[RegretTable<NlheGame6>; 6]` runtime 真实多表 deferred 到 E2（继承 D-321-rev1 → D-321-rev2 同型模式），D2 commit 维持 single shared RegretTable + traverser alternating；checkpoint v2 schema header `traverser_count=6` 已就位让 E2 真并发 6 套表 + body sub-region encoding 同 commit 翻面。(iii) **stage 3 SCHEMA_VERSION 字面更新** — `checkpoint_round_trip::d350_header_constants_lock::assert_eq!(SCHEMA_VERSION, 1)` 改 `assert_eq!(SCHEMA_VERSION, 2)`（D2 bump 字面顺势更新）。(iv) **C1 测试算术订正** — `checkpoint_v2_schema::checkpoint_header_field_size_addendum_32_bytes` 内 `new_fields_total + 12` 改 `new_fields_total`（订正 +12 误差，实际 HEADER_LEN bump = 4 u8 + 16-byte pad_b = 20 byte）。(v) **OOM step-path dispatch deferred 到 E2** — `TrainerError::OutOfMemory { rss_bytes, limit }` variant A1 已落地，D2 训练循环未接入 runtime trigger（`tests/training_24h_continuous.rs` 3 个 #[ignore] 测试走自带 RSS probe + panic-on-exceed，不依赖 step 路径触发 OOM variant，E2 metrics 接入后实际触发 dispatch）。
- **下一步**：**Stage 4 E1 [测试]** — `tests/perf_slo.rs::stage4_*` 8 条 SLO（D-490 单线程 5K / 4-core 15K / 32-vCPU 20K + D-454 LBR P95 30s + D-485 baseline eval 2min + D-461 24h continuous + D-498 7-day nightly fuzz + D-490 6-traverser cross-check） + `tests/lbr_eval_convergence.rs` 6 条（first usable 10⁹ update 后 LBR < 200 mbb/g / 100 采样点单调非升 / per-traverser 上界 D-459 / OpenSpiel-export byte-equal / 14-action LBR enumerate / D-455 myopic horizon=1）。**通过标准**：default profile 14 测试 panic-fail（E2 [实现] 落地后转绿）+ `--ignored` 触发 AWS c7a.8xlarge 实测 first usable 训练完成后达到 D-490 阈值。详见 `docs/pluribus_stage4_workflow.md` §E1。

历史出口数据、carve-out 全文、实测数字一律不在本文件保留。查阅顺序：

1. `docs/pluribus_stage{1,2,3}_report.md` — 各 stage 验收报告 + carve-out 索引（stage 3 报告 F3 后生成）。
2. `docs/pluribus_stage{1,2,3,4}_workflow.md` §修订历史 — 所有 §X-revN carve-out 全文（[测试]↔[实现] 越界追认、D-NNN-revM 翻语义、错误前移、procedural follow-through 等）。
3. `git log --oneline stage1-v1.0..` — stage-2 + stage-3 + stage-4 实施提交时间线。
4. `git show <commit>` — 单 commit 出口数据 + 实测数字。

### 当前 baseline / artifact ground truth

**Stage 1 baseline**（frozen at `stage1-v1.0`，stage-2+ D-272 不退化锚点）：

- `cargo test`（默认）：stage-1 部分 104 passed / 19 ignored / 0 failed across 16 test crates。
- `cargo test --release -- --ignored`：13 个 release ignored 套件全绿（代表性数字见 stage-1 报告 §F3）。
- `cargo test --release --test perf_slo -- --ignored`（1-CPU host）：4 active + 1 多核 skip-with-log；eval7 single 20.76M eval/s / simulate 134.9K hand/s / history encode 5.33M action/s / decode 2.51M action/s 等。

**Stage 2 baseline**（frozen at `stage2-v1.0`）：

- `cargo test --release --no-fail-fast`：282 passed / 0 failed / 45 ignored across 35 result sections（含 stage-1 16 integration crates byte-equal 维持）。release 全套 ~30 min（C2 bucket-table fixture 训练 250-775 s/each × 4 大头）。
- Stage 2 SLO：D-280 24.95M mapping/s（249× 余量） / D-281 P95 153 ns（~65× 余量） / D-282 vultr 50-run mean 1093.2 hand/s 50/50 PASS（主 host 1-CPU 单跑 borderline，§E-rev1 §5 / §F-rev1 §2 carve-out）。
- `cargo test --release --test abstraction_fuzz -- --ignored`：1M iter 3 个 full 套件 0 panic / 0 invariant violation。

**当前 artifact 基线**（§G-batch1 §3.10 落地后）：

- **v3 production** `artifacts/bucket_table_default_500_500_500_seed_cafebabe_v3.bin` — 528 MiB / 553,631,520 bytes / 不进 git history / body BLAKE3 `67ee555439f2c918698650c05f40a7a5e9e812280ceb87fc3c6590add98650cd`。生成路径：§3.9 single-phase full N + ClusterIter::production_default() (flop=2000/turn=5000/river=10000) + §3.10 river_exact 990 enumerate；AWS c6a.8xlarge 32-core EPYC 7R13 / 61 GB on-demand 1h 37m。CLAUDE.md ground truth 段以 v3 为准。
- **v2**（历史参照）body `e602f548...` / whole-file `211319ff...` — §3.4-batch2 dual-phase MC iter=2000 16-core 11h 47min 出口；CLAUDE.md ground truth 切到 v3 后从 default test path 退役。
- **v1**（历史参照）95 KB body `4b42bf70...` — §3.2 schema bump 1→2 后被 `BucketTable::open` 拒绝（SchemaVersionMismatch）。
- **Fixture artifact** body `a6989eeb1dc618ef8a6b375d6af1dcef547a96cdb2c0e84e4b6341562183c2b6` — `--mode fixture --flop 10 --turn 10 --river 10 --cluster-iter 100` smoke，跨多次 commit byte-equal 维持。
- **跨架构 baseline** `tests/data/bucket-table-arch-hashes-linux-x86_64.txt`（32-seed × 3 街 fixture content_hash）byte-equal 维持；darwin-aarch64 仍 aspirational（D-052）。

**v3 bucket quality 实测**：19 测试 10 passed / 9 failed（同 v2 模式，std_dev 改善 18-41%，EMD/monotonic 揭示 D-233-rev1 sqrt-scale K=500 偏紧 + D-236b MC reorder noise；详 `docs/pluribus_stage2_bucket_quality_v3_test_report.md`）。D-233-rev2 carve-out 等 stage 3 CFR exploitability 实测后决定。

### Build/test/lint commands

- `./scripts/setup-rust.sh` — idempotent rustup install；pins `rust-toolchain.toml`（`1.95.0`）。
- `cargo build --all-targets`
- `cargo fmt --all --check`
- `cargo clippy --all-targets -- -D warnings`
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
- `cargo test --no-run` — compile only。
- `cargo test` — 默认全绿。PokerKit 两条交叉验证在依赖不可用时自动 skipped。
- `cargo test --release -- --ignored` — full-volume 测试。
- `cargo bench --bench baseline` — stage-1 5 条 + stage-2 追加 3 条（`abstraction/info_mapping` / `equity_monte_carlo` / `bucket_lookup`）；CI 短路径 `--warm-up-time 1 --measurement-time 1 --sample-size 10 --noplot`，nightly 全量。
- `cargo bench --bench stage3` — stage-3 落地的 2 group（`kuhn_cfr_iter` / `leduc_cfr_iter`，C1 补 `nlhe_es_mccfr_update`）。
- `cargo test --release --test perf_slo -- --ignored` — 5 条 stage-1 + 3 条 stage-2 SLO 断言。

### 装 PokerKit（C2 实测可用）

```bash
uv venv --python 3.11 .venv-pokerkit
uv pip install --python .venv-pokerkit/bin/python "pokerkit==0.4.14"
PATH=".venv-pokerkit/bin:$PATH" cargo test                        # 默认 + active cross-validation
PATH=".venv-pokerkit/bin:$PATH" cargo test --release -- --ignored # full-volume
```

`.venv-pokerkit/` 已 gitignore。

## Documents and their authority

合约层次（按下列顺序读取）：

1. `docs/pluribus_path.md` — 8-stage roadmap + 各 stage 验收 gate + 硬件/时间预算。**stages 4-6 阈值严于原 Pluribus，不可弱化**。
2. Stage 1（locked at `stage1-v1.0`）：`pluribus_stage1_validation.md` / `pluribus_stage1_decisions.md`（D-001..D-103，**Authoritative spec for implementers**）/ `pluribus_stage1_api.md`（API-NNN，**Authoritative spec for testers**）/ `pluribus_stage1_workflow.md`。
3. Stage 2（locked at `stage2-v1.0`）：`pluribus_stage2_{validation,decisions,api,workflow}.md`（D-200..D-283 + API-200..API-302）。
4. Stage 3（locked at `stage3-v1.0`）：`pluribus_stage3_{validation,decisions,api,workflow}.md`（D-300..D-379 + API-300..API-392）。
5. Stage 4（A0 closed 2026-05-14）：`pluribus_stage4_{validation,decisions,api,workflow}.md`（D-400..D-499 + API-400..API-499）。

变更影响决策/API 签名走 **D-NNN-revM / API-NNN-revM** flow（`pluribus_stage1_decisions.md` §10 + `pluribus_stage1_api.md` §11）：append rev entry，不删原条，serialization 受影响则 bump `HandHistory.schema_version`。历史 rev 索引（全文见各文档 §10/§11 修订历史）：

- D-022b-rev1 — `n_seats == 2` heads-up 走标准 HU NLHE 语义（button=SB / non-button=BB / postflop OOP 先行）；新增 `first_postflop_actor() = next_seat(button)` universal rule；n_seats>=3 路径 byte-equal 不变。
- D-033-rev1 — incomplete raise 不重开 raise option，TDA Rule 41 / PokerKit 对齐；per-player `raise_option_open: bool`。
- D-039-rev1 — odd-chip 余 chip 整笔给按钮左侧最近获胜者（PokerKit chips-pushing divmod 语义）。
- D-037-rev1 — `last_aggressor` 作用域收紧到「最后一条 betting round 内」（PokerKit `_begin_betting` 每街起手清 `opener_index`）。
- API-001-rev1 — `HandHistory::replay` / `replay_to` 返回 `Result<_, HistoryError>`，wraps `RuleError`。
- API-004-rev1（stage 2 B2 触发）— `GameState::config()` additive 只读 getter（D-211-rev1 `stack_bucket` 来源）。
- API-005-rev1（stage 2 E2 触发）— `RngSource::fill_u64s(&mut self, dst: &mut [u64])` additive default-impl；byte-equal 与循环 `next_u64` 等价（D-051 / D-228 / D-237 满足）。
- D-321-rev1（stage 3 C2 触发）— ES-MCCFR thread-safety = thread-local accumulator + batch merge（候选 ③）；C2 ship serial-equivalent step_parallel；真并发 deferred 到 E2（详 `docs/pluribus_stage3_decisions.md` §10.2）。
- D-317-rev1（stage 3 C2 触发）— 简化 NLHE InfoSetId 在 stage 2 `bucket_id` field bits 12..18 编码 6-bit `legal_actions` availability mask 让 D-324 成立；IA-007 reserved 区域不受影响（详 `docs/pluribus_stage3_decisions.md` §10.3）。

## Workflow (multi-agent, strict role boundaries) — applies to all stages

Each stage 组织为 `A → B → C → D → E → F`（13 步）。每步 tag `[决策] / [测试] / [实现] / [报告]`，**role boundaries enforced**：

- `[测试]` agent 只写 tests / harness / benchmarks。**不修改产品代码**。测试暴露 bug → file issue 给 `[实现]`。
- `[实现]` agent 只写产品代码。**不修改测试**。测试 fail 改产品代码；测试有明显 bug 才改测试，且 review 后。
- `[决策]` / `[报告]` 产出或修改 `docs/`。

任务到来时先识别 stage + step（A0/A1/B1/…），按角色操作。历史角色越界 carve-out（[测试]↔[实现] 边界破例追认 / 0 产品代码改动也算 closure / D-NNN-revM 翻语义同 commit 翻测试 / 错误前移单点不变量）逐条记录在各 stage `pluribus_stage{N}_workflow.md` §修订历史；遇相似情况直接查那三份文档。

## Non-negotiable invariants

These are repeated across decision and validation docs because violations corrupt downstream CFR training and are nearly impossible to debug post-hoc:

- **No floating point in rules, evaluator, history, or abstraction.** Chips are integer `u64` (`ChipAmount`); P&L is `i64`. PR 引入 `f32`/`f64` 在这些路径必须 reject（D-026）。
- **No global RNG.** All randomness goes through an explicit `RngSource` parameter (D-027, D-050).
- **No `unsafe`.** `Cargo.toml [lints.rust] unsafe_code = "forbid"` 编译期拒绝。若觉得需要，必是设计问题，escalate。
- **`ChipAmount::Sub` panics on underflow** in both debug and release (D-026b)。需要 saturating 用 `checked_sub` 显式。
- **`Action::Raise { to }` is an absolute amount**, not a delta — matches NLHE protocol convention.
- **`SeatId(k+1 mod n_seats)` is the left neighbor of `SeatId(k)`** (D-029)。所有「向左」/「按钮左」引用（button rotation D-032, blinds D-022b, odd-chip D-039, showdown order D-037, deal start D-028）用此单一方向约定。
- **`RngSource → deck` dealing protocol is a public contract** (D-028)。Fisher-Yates over `[Card::from_u8(0..52)]` 消耗 exactly 51 `next_u64` calls + 固定 deck-index → hole/board mapping。Testers 可构造 stacked `RngSource` 实现来 exercise 协议；实现不可偏离。改变 bump `HandHistory.schema_version`。
- **Showdown `last_aggressor`** 仅计 voluntary bets/raises（blinds, antes, preflop limps 不算）(D-037, D-037-rev1)。
- **Short all-in does not reopen raise option** — but only for **already-acted** players（D-033, **D-033-rev1**, validation §1）。Per D-033-rev1（TDA Rule 41 alignment）：incomplete raise 不（a）更新 `last_full_raise_size` 也不（b）改任何玩家 `raise_option_open`。Flag `true` 玩家（未对前次 full raise 行动）保持 `true` 仍可 raise；flag 已 `false`（已行动）的玩家直到后续 full raise reopen 才能 raise。
- **Determinism baseline:** same architecture + toolchain + seed → identical hand-history BLAKE3 hash. 跨架构（x86 vs ARM）aspirational，非 stage-1 pass criterion（D-051, D-052）。
- **`tests/api_signatures.rs` is the spec-drift trip-wire.** A1 stubs 返回 `!` 与任意返回类型 unify — 否则错签名 silently compile。任何 `pluribus_stage{1,2,3}_api.md` 签名改动（via API-NNN-revM）必须同 PR 更新此文件；否则 `cargo test --no-run` fail。
- **`canonical_observation_id` 对 (board, hole) 集合的任意输入顺序不变** (D-218-rev1 / §C-rev2 §4)。`postflop.rs` 在 first-appearance suit remap 之前按 `Card::to_u8()` 升序排序 board / hole；`tests/canonical_observation.rs::canonical_observation_id_input_shuffle_invariance_*` 是 regression guard。

## Stage 2 / Stage 3 API surface（继续约束 stage 3+ work）

Stage 2 输出（详见 `pluribus_stage2_api.md` + 报告 §11）：`DefaultActionAbstraction` / `PreflopLossless169` / `PostflopBucketAbstraction` / `MonteCarloEquity` / `BucketTable` + `BucketTableError` / `InfoSetId` (64-bit) / `BettingState` / `StreetTag` / `InfoAbstraction` trait / `cluster::rng_substream::*`（sub-stream op_id 表 + `derive_substream_seed` D-228）/ `TrainingMode { Fixture, Production }` + `train_in_memory_with_mode(...)`。

Stage 3 A1 scaffold 暴露（详见 `pluribus_stage3_api.md`）：`Game` trait + `KuhnGame` / `LeducGame` / `SimplifiedNlheGame` impl，`Trainer<G: Game>` trait + `VanillaCfrTrainer` / `EsMccfrTrainer`，`RegretTable` / `StrategyAccumulator`，`KuhnBestResponse` / `LeducBestResponse`，`Checkpoint`，sampling op_id 常量。B2 已落地 Vanilla CFR + Kuhn/Leduc 全 trait 方法；C2 落地 SimplifiedNlheGame + EsMccfrTrainer；D2 落地 checkpoint。

stage 1 + stage 2 不变量与反模式继续约束 stage 3。

## Engineering anti-patterns (explicit in workflow docs)

When proposing changes, do not:

- Optimize before correctness — performance lives in step E2，不是 B2/C2。Naive evaluator 在 B2/C2 跑 10k eval/s 是 intentional（D-073）。
- Pre-abstract with traits/generics "for future extension" in A1 / B2。
- Skip the cross-validation harness — PokerKit 必须 wired in **at B1**，不延迟到 C1。
- Write all 200+ scenarios up front — B1 写 10 driving scenarios；C1 batches the rest。
- Split into multiple crates early — single crate, multi-module 直到 C2 stabilizes the API（D-010..D-012）。
- Assume our implementation is correct when it diverges from PokerKit。Default assumption: our bug。Only after review may a divergence be recorded as a reference-implementation difference（D-083）。

## Working language

Docs and commit messages in this repo are in Chinese. Match the existing tone and language when adding to `docs/` or writing commits. Code identifiers and inline comments should be English (Rust convention).

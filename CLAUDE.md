# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository status

Stage 1 of an 8-stage Pluribus-style 6-max NLHE poker AI. **Stage 1 closed**（F3 [报告] is done）：13 步全部按 workflow §修订历史 时间线闭合（B-rev1 / C-rev1 / C-rev2 / D-rev0 / E-rev0 / E-rev1 / F-rev0 / F-rev1 / F-rev2 = 9 条修订记录）；阶段 1 出口检查清单可在单核 host 落地的项目全部归零，剩余 3 项 carve-out 与代码合并解耦（多核 host efficiency 实测 / 完整 100k cross-validation 实跑时间戳 / 24h 夜间 fuzz 7 天连续）。验收报告 `docs/pluribus_stage1_report.md` 落地；git tag `stage1-v1.0` 标定 stage-1 闭合 commit。123 个 `#[test]` 函数 across 16 test crates，默认 `cargo test` 104 active / 19 ignored / 0 failed；`cargo test --release -- --ignored` 19/19 全绿（F3 实测：5 SLO 断言 20.76M eval/s / 134.9K hand/s / 5.33M encode / 2.51M decode + 1M fuzz / 1M determinism / 100k roundtrip / 10k cross-lang 全绿）。**前置 F2 ([实现]) is done**（详见 §修订历史 F-rev1）。

F3 出口数据（截至本 commit；PATH 含 `.venv-pokerkit`/`python3.11` + PokerKit 0.4.14；本机 1-CPU AMD64）：

- 验收报告 `docs/pluribus_stage1_report.md` 落地；git tag `stage1-v1.0` 标定本 commit。详见报告 §1-§10。
- `cargo test`（默认 / debug profile）：**104 passed / 19 ignored / 0 failed across 16 test crates**（123 个 `#[test]` 函数）。
- `cargo test --release --test perf_slo -- --ignored --nocapture`：5 SLO 断言全绿。eval7 single thread 20,759,014 eval/s（≥10M，2.08× 余量）；eval7 multithread skip-with-log（1-CPU host carve-out）；simulate 134,909 hand/s（≥100K，1.35× 余量）；history encode 5,328,565 action/s（≥1M，5.33× 余量）；history decode 2,513,781 action/s（≥1M，2.51× 余量）。
- `cargo test --release --test history_roundtrip --test cross_lang_history --test history_corruption --test fuzz_smoke --test determinism --test evaluator -- --ignored`：13 个 release ignored 套件全绿。`fuzz_d1_full_1m_hands_no_invariant_violations` 1M/1M ok 11.48s；`determinism_full_1m_hands_multithread_match` 1M/1M ok 29.46s；`history_roundtrip_full_100k` 100k/100k ok 3.20s；`cross_lang_full_10k` 10k/10k ok 4.95s；`history_corruption` 4/4 ok 0.43s（含 100k byte flip）；evaluator 1M three-piece 三件套合计 2.30s。
- `cargo fmt --all --check` / `cargo clippy --all-targets -- -D warnings` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`：全绿。
- F3 角色边界审计：仅写新文件 `docs/pluribus_stage1_report.md` + 修订 `docs/pluribus_stage1_workflow.md` §F-rev2 + `CLAUDE.md` 状态翻 stage 1 closed；`src/`、`tests/`、`benches/`、`fuzz/`、`tools/`、`proto/` **未修改一行**——[报告] role 0 越界。

前置 F2 出口数据（保留对照）：

- `cargo test --test history_corruption -- --ignored`：**4 passed / 0 failed**（F1 出口为 1 passed / 3 failed）。`from_proto_rejects_action_seat_out_of_range` / `_button_seat_out_of_range` / `_duplicate_card_in_board` 三条 carry-over 全部从 `replay()` 阶段错误前移到 `from_proto` 阶段，返回 `HistoryError::Corrupted`。`byte_flip_no_panic_full_100k` 仍 0 panic。
- F2 角色边界审计：仅 `src/history.rs` 单文件加 5 处校验 + `docs/pluribus_stage1_workflow.md` §F-rev1 + `CLAUDE.md` 状态同步；`tests/`、`benches/`、`fuzz/`、`tools/`、`proto/` **未修改一行**——0 越界。

前置 F1 出口数据（保留对照；本机 1-CPU AMD64 debug profile）：

- `cargo test`（默认 F1 时点）：**104 passed / 19 ignored / 0 failed across 16 test crates**；总耗时 ~55s（cross_validation 100-hand vs PokerKit dominates）。F1 三件套出口：`schema_compat` 10 active / 0 ignored；`history_corruption` 23 active / 4 ignored（4 条 = F1→F2 carry-over：3 条 from_proto 严校验前移候选 + 1 条 100k fuzz opt-in）；`evaluator_lookup` 8 active / 0 ignored。
- F1 角色边界审计：`tests/schema_compat.rs` / `tests/history_corruption.rs` / `tests/evaluator_lookup.rs` 三新文件 + `docs/pluribus_stage1_workflow.md` §F-rev0 + `CLAUDE.md` 状态同步；`src/`、`benches/`、`fuzz/`、`tools/`、`proto/` **未修改一行**——0 越界（与 §C-rev1 / §E-rev1 「常规闭合 + 0 越界」 同型）。

前置 E2 出口数据（保留对照；本机 1-CPU AMD64 release profile）：

- `cargo test`（默认 E2 时点）：63 passed / 10 ignored / 0 failed across 13 crates；耗时 ~10s（E1/D2 时 ~50–60s）。100 手规则交叉验证 + 1k PokerKit category 交叉验证均 0 diverged。
- `cargo test --release --test perf_slo -- --ignored --nocapture`：5/5 全绿。`slo_eval7_single_thread` 21.2 M eval/s（SLO ≥ 10 M，2.1× 余量）；`slo_eval7_multithread_linear_scaling_to_8_cores` skip-with-log（1-CPU host，efficiency 断言留多核 carve-out）；`slo_simulate_full_hand` 192.4 K hand/s（SLO ≥ 100 K，1.92× 余量）；`slo_history_encode` 4.96 M action/s；`slo_history_decode` 2.38 M action/s。
- `cargo bench --bench baseline -- --warm-up-time 1 --measurement-time 1 --sample-size 10 --noplot`：criterion 自动比对 E1 baseline，eval7 single_call **+10909%**（24.9 M eval/s），eval7 batch_1024 **+16337%**（31.2 M eval/s），simulate **+273%**（162.7 K hand/s），history encode/decode 与基线持平不回归。
- `cargo test --release -- --ignored`（不含 100k cross-validation——见下文 carve-out）9 个 full-volume 全绿且大幅加速：1M three-piece naive evaluator（`eval_5_6_7_consistency_full` + `eval_antisymmetry_stability_full` + `eval_transitivity_full`）合计 **1.97s**（D2 时 46.69s，~24×）；`cross_eval_full_100k`（PokerKit category 100k）100,000/100,000 match，0 diverged，50.87s（PokerKit Python 子进程 dominates，本侧加速被 RTT 吞噬，无回归）；`cross_lang_full_10k` 10,000/10,000 0 diverged，4.13s；`history_roundtrip_full_100k` 100,000/100,000 ok，**2.48s**（D2 时 8.19s，~3.3×）；`determinism_full_1m_hands_multithread_match` 1M/1M 0 哈希分歧，**24.68s**（D2 时 121s，~5×）；`fuzz_d1_full_1m_hands_no_invariant_violations` 1,000,000 hands 0 violations / 0 panics，**9.29s**（D2 时 77.81s，~8×）；`cross_arch_hash_capture_only` ok。
- `cargo fmt --all --check` / `cargo clippy --all-targets -- -D warnings` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`：全绿。
- **未实跑（carve-out 同 E-rev0/D-rev0 形态）**：(a) `slo_eval7_multithread_linear_scaling_to_8_cores` efficiency ≥ 0.70 的多核 host 实测（断言代码 + skip 路径就位，留待 ≥2 核 host）；(b) 完整 100k cross-validation（本机 1-CPU 上单进程串行 ~14h，每手一个 Python 子进程；E2 不改变规则引擎语义，`HandRank` 数值字节级与朴素实现一致，D2 commit `023d470` 修复的 105 条 historical divergent seeds 在 E2 commit 仍 0 diverged，建议多核 host 跑一次产出 0-diverged 实测时间戳）；(c) 24h 夜间 fuzz 7 天连续无 panic（self-hosted runner 解耦运行）。三项都与代码合并解耦。

**Step D1 (`[测试]` agent — fuzz 完整版 + 多线程测试) is closed**：commits `bc75598..2ea667b` 落地 1M fuzz / 多线程 1M / cargo-fuzz 子 crate / cross-arch baseline / CI fuzz-quick / nightly fuzz / C-rev1 carve-out 测试 + per-divergence eprintln。100k cross-validation 实跑结果（105 条分歧）已入账于 `docs/xvalidate_100k_diverged_seeds.md`，由 D2 [实现] 闭合。详见 workflow §修订历史 C-rev2。

**Step C2 ([实现]) closed with 0 lines of product-code change** — C1 had already left the default suite green and the §C2 [实现] §输出 列表的 5 条产品代码任务在 B2/C1 顺序里逐项落地完毕；C2 的实际动作是在装好 PokerKit 0.4.14 的环境把 C1 留下的 6 个 `#[ignore]` full-volume 门槛跑一遍并书写 closure。详见 `docs/pluribus_stage1_workflow.md` §修订历史 C-rev1。C1 状态保留如下：B2 had landed the full product side (state machine / evaluator / history) and 17 driving tests; C1 ([测试] agent) layered the §C1 acceptance harness on top **without touching product code**:

- `tests/common/mod.rs` — extended scenario DSL: `ScenarioCase` + `ScenarioExpect` (含 `LegalAtEndCheck` enum) + `run_scenario` driver，使每个 fixed scenario 5–10 行表达。
- `tests/scenarios_extended.rs` — 234 fixed scenarios（≥200 门槛达成）含 67 short-allin / incomplete raise（≥50 门槛）、min-raise 链条 / 摊牌顺序 / 拒绝路径覆盖。D-033-rev1 already-acted vs still-open 两条路径在不同 stack 大小下系统化扫描。
- `tests/side_pots.rs` — side pot / split pot 110+ scenarios（≥100 门槛）含 25 uncalled bet returned 路径（≥20 门槛）、odd-chip-给-SB 12 例（D-039-rev1）、4-way side pot 17 例、5-way side pot 9 例、dead money 8 例；用 stacked-deck "BB 必胜 quads" 模板让 stack 结构一表一格生成。
- `tests/evaluator.rs` — 10 类 HandCategory 公开样例 + 类型间相对强度 + 5/6/7-card 接口一致性 + 反对称/稳定性 + 传递性。默认 5k–10k 量级；`#[ignore]` 提供 1M full-volume opt-in（`cargo test -- --ignored`）。
- `tests/cross_eval.rs` + `tools/pokerkit_eval.py` — 评估器 vs PokerKit 交叉验证 harness。**比对粒度仅为 `HandCategory`（0..9 共 10 类枚举）**，不含完整 5-best 名次（rank tuple）；rank 比对留到 E2 高性能评估器接入后并入 1M 回归（见 `validation.md` §4 修订历史 2026-05-08 与 D-085）。默认 1k 手；`#[ignore]` 100k（与 D-085 C2 通过门槛对齐）；E2 后扩到 1M。PokerKit 缺失时 skipped。
- `tests/history_roundtrip.rs` — proto serialize → deserialize → `replay()` 全字段 + `content_hash` 一致；默认 1k 手；`#[ignore]` 100k。`replay_to(k)` 中间态 50 个 seed × 全 index 验证。
- `tools/history_reader.py` + `tests/cross_lang_history.rs` — Python minimal proto3 decoder（无 protoc 依赖）读 Rust 写出的 history protobuf。默认 100 手；`#[ignore]` 10k（已实跑 0 分歧）。
- `tests/determinism.rs` — 同 seed 重复 10 次哈希相同（20 个 seed）+ 单线程 vs 4 线程批量内容一致（200 seeds）+ 不同 seed 哈希足够分散 + `to_proto` 重复字节稳定。
- `Cargo.toml` 新增 dev-dep `base64 = "0.22"`（C1 跨语言 harness 的 stdin 编码；test-only，不进产品二进制）。
- D-033-rev1 / D-039-rev1 / API-001-rev1 文档不动；C1 没有触发任何 D-NNN-revM / API-NNN-revM。
- **D-037-rev1**（D2 [实现] 落地）：`last_aggressor` 作用域从「整手最后一次 voluntary bet/raise」收紧到「**最后一条 betting round 内**最后一次 voluntary bet/raise」；与 PokerKit 0.4.14 `_begin_betting` (state.py:3381) 每条街起手清 `opener_index` + `Opening.POSITION` 默认回到 SB 的语义一致。详见 `docs/pluribus_stage1_decisions.md` §10 修订历史。`tests/scenarios.rs::last_aggressor_shows_first` + `tests/scenarios_extended.rs::showdown_order_table` case (a) 同 commit 翻断言到新语义，[实现] 越界以 workflow §修订历史 D-rev0 carve-out 追认。

C2 出口数据（截至本仓库 commit；PATH 含 `.venv-pokerkit`/`python3.11` + PokerKit 0.4.14）：

- `cargo test`（默认）：61 passed / 6 ignored / 0 failed across 12 crates；耗时 ~50s。先前在无 PokerKit 时 skipped 的两条交叉验证现 active：
    - `cross_validation_pokerkit_100_random_hands`（100 手规则引擎 vs PokerKit）：100/100 match，0 diverged。
    - `cross_eval_smoke_default`（1k 手 HandCategory vs PokerKit）：1000/1000 match，0 diverged。
- `cargo test --release -- --ignored` 跑齐 6 个 full-volume：
    - `cross_eval_full_100k`（D-085 评估器侧 C2 通过门槛）：100,000/100,000 match，0 diverged，41.82s。
    - `cross_lang_full_10k`：10,000/10,000 match，0 diverged，4.48s。
    - `history_roundtrip_full_100k`：100,000/100,000 ok，8.19s。
    - `eval_5_6_7_consistency_full` / `eval_antisymmetry_stability_full` / `eval_transitivity_full`（1M naive evaluator 三件套）：合计 46.69s 全绿。注意此 1M 不等于 validation.md §4 的「评估器 vs PokerKit 1M」（E2 aspirational），它们是 naive evaluator 自洽性 / 反对称 / 传递，不涉及参考实现。
- `cargo fmt --all --check` / `cargo clippy --all-targets -- -D warnings` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`：全绿。

**C2/D2 carve-out 现状**：D-085 / validation.md §7 要求「规则与 PokerKit 100,000 手 0 分歧」。

- **测试侧**：C2 闭合时仅有 100 手规模；D1 [测试] 在 commit `bc75598` 加了 `cross_validation_pokerkit_100k_random_hands` `#[ignore]` + `scripts/run-cross-validation-100k.sh`（N chunk 并行）。**测试缺位已闭合**。
- **0 分歧门槛**：D1 [测试] 第一次实跑（commit `2ea667b`，N=8 × 12,500 hand）暴露 105 条产品代码分歧；D2 [实现] 在本 commit 修复完，105 条历史 divergent seeds 单独跑全部通过。**0 分歧门槛在已知 seed 集上闭合**；完整 100k 实跑因本机 1-CPU 限制留待多核 host 验证（详见 `docs/pluribus_stage1_workflow.md` §修订历史 D-rev0）。

Build/test/lint commands are valid as of C2 closure:

- `./scripts/setup-rust.sh` — idempotent rustup install. Pins to `rust-toolchain.toml` (currently `1.95.0`).
- `cargo build --all-targets`
- `cargo fmt --all --check`
- `cargo clippy --all-targets -- -D warnings`
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
- `cargo test --no-run` — compile tests. F1 闭合后 ships **123 tests across 16 crates**：`api_signatures` (1) + `cross_arch_hash` (2+1 ignored) + `cross_eval` (1+1 ignored) + `cross_lang_history` (1+1 ignored) + `cross_validation` (3+1 ignored) + `determinism` (4+1 ignored) + `evaluator` (8+3 ignored) + `evaluator_lookup` (8) + `fuzz_smoke` (3+1 ignored) + `history_corruption` (23+4 ignored) + `history_roundtrip` (3+1 ignored) + `perf_slo` (0+5 ignored) + `scenarios` (10) + `scenarios_extended` (19) + `schema_compat` (10) + `side_pots` (8)。
- `cargo test` — 默认 **104/104 全绿**（19 条 `#[ignore]`：5 perf_slo SLO + 4 history_corruption F1→F2 carry-over + 10 其他 full-volume opt-in；必须显式触发）。需要外部依赖的两条交叉验证 (`cross_eval` 类别 vs PokerKit / `cross_validation` 100-hand) 在 `pokerkit==0.4.14` + Python ≥3.11 不可用时自动 skipped；F1 闭合时已在 `.venv-pokerkit`/`python3.11` 环境实跑确认 0 分歧。
- `cargo test --release -- --ignored` — 10 个 full-volume 测试 D2 闭合时实跑：C2 落地的 6 件套（cross_eval_full_100k 41.82s / cross_lang_full_10k 4.48s / history_roundtrip_full_100k 8.19s / 1M naive evaluator 三件套合计 46.69s）+ D1 落地的 4 件套（fuzz_d1_full_1m 77.81s / determinism_full_1m 121s / cross_arch_hash_capture_only / cross_validation_pokerkit_100k_random_hands `#[ignore]` 全 100k 待多核 host 实跑）。性能 SLO（≥10M eval/s）仍在 E1/E2。
- `cargo bench --bench baseline` — E1 落地 5 个 bench：`eval7_naive/{single_call, batch_1024_unique_hands}` + `simulate/random_hand_6max_100bb` + `history/{encode, decode}`。每个 bench 用 `Throughput::Elements` 把单位钉到 ops/s（eval/s / hand/s / action/s），可直接对照 validation §8 SLO 数字。CI 短路径走 `--warm-up-time 1 --measurement-time 1 --sample-size 10 --noplot` 整 job ~18s（`.github/workflows/ci.yml::bench-quick`）；nightly 全量默认参数 + 上传 `target/criterion/` artifact（`.github/workflows/nightly.yml::bench-full`）。**bench 文件本身不做阈值断言**——SLO 阈值断言在 `tests/perf_slo.rs`，需 release profile 与 `--ignored` 显式触发。
- `cargo test --release --test perf_slo -- --ignored` — E1 落地 5 条 SLO 阈值断言。**E2 closure 实测 5/5 全绿（1-CPU host）**：`slo_eval7_single_thread_at_least_10m_per_second` 21.2M eval/s（PASS，SLO ≥ 10M，2.1× 余量）；`slo_eval7_multithread_linear_scaling_to_8_cores` 1-CPU host skip（pass with log，多核 carve-out 留待 ≥2 核 host）；`slo_simulate_full_hand_at_least_100k_per_second` 192.4K hand/s（PASS，1.92× 余量）；`slo_history_encode_at_least_1m_action_per_second` 4.96M action/s（PASS）；`slo_history_decode_at_least_1m_action_per_second` 2.38M action/s（PASS）。多核 host 上 efficiency ≥ 0.70 的多线程断言验收留待 ≥2 核 host follow-up。

**装 PokerKit 的标准流程**（C2 实测可用，留作后续 [测试] / [实现] agent 复用；外部 Python ≥3.11 需求来自 `tools/pokerkit_eval.py` 与 `tools/pokerkit_replay.py`）：

```bash
uv venv --python 3.11 .venv-pokerkit
uv pip install --python .venv-pokerkit/bin/python "pokerkit==0.4.14"
PATH=".venv-pokerkit/bin:$PATH" cargo test                      # 默认 + active cross-validation
PATH=".venv-pokerkit/bin:$PATH" cargo test --release -- --ignored  # full-volume 6 件套
```

`.venv-pokerkit/` 已在 `.gitignore` 中 ignore。

Stages 2–8 source code does not exist yet. **Stage 1 is closed**（git tag `stage1-v1.0`，验收报告 `docs/pluribus_stage1_report.md`）；stage-2 起步阅读顺序见报告 §10。stage-1 与代码合并解耦的 follow-up 项（不阻塞 stage-2 起步、与 stage-2 实施可并行）：(a) 完整 100k cross-validation 在多核 host 实跑产出 0 diverged 时间戳（D-rev0 / E-rev1 carve-out；105 historical divergent seeds 在 stage-1 闭合 commit 0 diverged 已是稳定证据）；(b) 24h 夜间 fuzz 在 self-hosted runner 连续 7 天无 panic / 无 invariant violation（D2 出口；`.github/workflows/nightly.yml` 已落地 GitHub-hosted matrix）；(c) `slo_eval7_multithread_linear_scaling_to_8_cores` 在 ≥2 核 host 上跑出 efficiency ≥ 0.70 实测数据（E-rev0 carve-out，E-rev1 继承）。三项都与代码合并解耦。

## Documents and their authority

The four stage-1 docs form a contract hierarchy. Read them in this order before making changes:

1. `docs/pluribus_path.md` — overall 8-stage roadmap, stage acceptance gates, hardware/time budgets. Stages 4–6 thresholds are deliberately stricter than the original Pluribus path; do **not** weaken them.
2. `docs/pluribus_stage1_validation.md` — quantitative pass criteria for stage 1 (e.g. 1M-hand fuzz, ≥10M eval/s, 100k-hand cross-validation with PokerKit).
3. `docs/pluribus_stage1_decisions.md` — locked technical/rule decisions (D-001 … D-103). **Authoritative spec for implementers.**
4. `docs/pluribus_stage1_api.md` — locked Rust API contract (API-NNN). **Authoritative spec for testers.**

If a change affects a decision or API signature, you must follow the **D-100 / API-NNN-revM** amendment flow described in `pluribus_stage1_decisions.md` §10 and `pluribus_stage1_api.md` §11 — append a `D-NNN-revM` / `API-NNN-revM` entry, never delete the original, and bump `HandHistory.schema_version` if serialization is affected. Both docs have a "修订历史" subsection. Past rev entries:

- **D-033-rev1** (decisions §10) — pin "incomplete raise 不重开 raise option" to TDA Rule 41 / PokerKit-aligned semantics: per-player `raise_option_open: bool`, full raise opens for un-acted players + closes raiser, call/fold closes self only, incomplete touches no flags. Drives `tests/scenarios.rs` #3 (already-acted SB → `raise_range = None`) vs #4 (still-open BTN → `raise_range = Some(min_to=650)`). validation.md §1 第 22 行措辞同步收紧。
- **D-039-rev1** (decisions §10) — odd-chip 余 chip 由「逐 1 chip 沿按钮左侧分配」改为「**整笔给按钮左侧最近的获胜者**」，对齐 PokerKit 0.4.14 默认 chips-pushing divmod 语义。每个 pot 仍独立计算；不同 pot 之间互不影响。`payouts()` 行为变化但公开签名不变；`HandHistory.schema_version` 不 bump（序列化格式未动）；`pluribus_stage1_validation.md` §3 同步。该 rev 在 B2 cross-validation 100 手 vs PokerKit 出现 1-chip 分歧后落地，遵循 workflow §B2 「默认假设我方理解错了规则」原则。
- **API-001-rev1** (api §11) — `HandHistory::replay` / `replay_to` return `Result<_, HistoryError>` instead of `RuleError`; `HistoryError::Rule { index, source: RuleError }` wraps the underlying rule error.

## Stage-1 workflow (multi-agent, strict role boundaries)

Stage 1 work is organized as `A → B → C → D → E → F` (13 steps, see `docs/pluribus_stage1_workflow.md`). Every step is tagged `[决策] / [测试] / [实现] / [报告]` and **role boundaries are enforced**:

- `[测试]` agent writes tests / harness / benchmarks only. **Never modify product code.** If a test reveals a bug, file an issue for `[实现]` to fix.
- `[实现]` agent writes product code only. **Never modify tests.** If a test fails, fix the product code; only edit the test if it has an obvious bug, and only after review.
- `[决策]` and `[报告]` produce or modify docs in `docs/`.

When the user asks you to do stage-1 work, identify which step (A0 / A1 / B1 / …) the task belongs to and operate within that role. **Stage 1 is closed**（F3 [报告] is done）：所有 13 步按 workflow §修订历史 时间线闭合；下一步是 stage 2 起步（参见 `docs/pluribus_path.md` §阶段 2）。历史关键边界事件：(1) B2 closure crossed the [实现]→[测试] boundary by completing two test files that B1 had deliberately left as stubs — see workflow §修订历史 B-rev1; (2) C2 closure carved out 「规则引擎 100k cross-validation 测试」 留给 [测试] agent — see §C-rev1; (3) D1 [测试] 实跑 100k cross-validation 暴露 105 条产品代码分歧 — see §C-rev2; (4) D2 [实现] 修完 105 条分歧 + 同 commit 翻新 2 条 scenario 测试到 D-037-rev1 语义；越界以 carve-out 追认 — see §D-rev0; (5) E1 [测试] 0 越界落地 bench harness + SLO 断言；多核多线程 SLO 在 1-CPU host 走 skip-with-log 留给多核 follow-up — see §E-rev0; (6) E2 [实现] 0 越界把 5/5 SLO 断言全部转绿（多线程在 1-CPU host 走 skip-with-log 不变）；apply 路径去 clone + 评估器换 bitmask 顺带让 1M fuzz / 1M determinism / 1M three-piece evaluator 等正确性测试加速 5–24× — see §E-rev1; (7) F1 [测试] 0 越界落地 schema 兼容 / corrupted history / evaluator lookup 三件套；评估器 lookup-table 加载失败路径在 E2 const-baked 设计下结构性缺位，F1 用结构性断言 + 黑盒完备性扫描间接覆盖；4 条域违规走 `#[ignore]` 留给 F2 自由 trade-off — see §F-rev0; (8) F2 [实现] 0 越界 trade-off 选择 「错误前移到 from_proto」，仅触 `src/history.rs` 一文件加 5 处域校验，4/4 F1→F2 carry-over `--ignored` 全部翻绿，「decode 后 seat 全部 < n_seats」 成单点不变量 — see §F-rev1; (9) F3 [报告] 0 越界落地 `docs/pluribus_stage1_report.md` + git tag `stage1-v1.0`；阶段 1 出口检查清单可在单核 host 落地的项目全部归零，剩余 3 项 carve-out 与代码合并解耦 — see §F-rev2.

## Non-negotiable invariants (apply to all stage-1 code)

These are repeated across the decision and validation docs because violations corrupt downstream CFR training and are nearly impossible to debug post-hoc:

- **No floating point in rules, evaluator, history, or abstraction.** Chips are integer `u64` (`ChipAmount`); P&L is `i64`. A PR that introduces `f32`/`f64` in these paths must be rejected (D-026).
- **No global RNG.** All randomness goes through an explicit `RngSource` parameter (D-027, D-050).
- **No `unsafe`.** `Cargo.toml [lints.rust] unsafe_code = "forbid"` rejects it at compile time. If you think you need it, escalate — almost certainly a design issue.
- **`ChipAmount::Sub` panics on underflow** in both debug and release (D-026b). Callers needing saturating semantics must use `checked_sub` explicitly.
- **`Action::Raise { to }` is an absolute amount**, not a delta — matches NLHE protocol convention.
- **`SeatId(k+1 mod n_seats)` is the left neighbor of `SeatId(k)`** (D-029). Every "向左" / "按钮左" reference (button rotation D-032, blinds D-022b, odd-chip D-039, showdown order D-037, deal start D-028) uses this single direction convention.
- **`RngSource → deck` dealing protocol is a public contract** (D-028). Fisher-Yates over `[Card::from_u8(0..52)]` consuming exactly 51 `next_u64` calls + fixed deck-index → hole/board mapping. Testers may construct stacked `RngSource` impls that exercise this protocol; implementation must not deviate. Any change bumps `HandHistory.schema_version`.
- **Showdown `last_aggressor`** counts only voluntary bets/raises (blinds, antes, preflop limps don't count) (D-037).
- **Short all-in does not reopen raise option** — but only for **already-acted** players. This is the most error-prone NLHE rule (D-033, **D-033-rev1**, validation §1). Per D-033-rev1 (TDA Rule 41 alignment): incomplete raises do not (a) update `last_full_raise_size` or (b) modify any player's `raise_option_open` flag. Players whose flag was `true` before the incomplete (un-acted on the prior full raise) keep it `true` and can still raise; players whose flag was already `false` (already-acted) cannot raise until a subsequent full raise reopens. `tests/scenarios.rs` #3 (`short_allin_does_not_reopen_raise`, SB-after-BTN-call) and #4 (`min_raise_chain_after_short_allin`, BTN-after-BB-incomplete) cover both branches.
- **Determinism baseline:** same architecture + toolchain + seed → identical hand-history BLAKE3 hash. Cross-architecture (x86 vs ARM) is an aspirational goal, not a stage-1 pass criterion (D-051, D-052).
- **`tests/api_signatures.rs` is the spec-drift trip-wire.** A1 stubs return `!` which unifies with any return type — wrong signatures compile silently otherwise. Any signature change in `pluribus_stage1_api.md` (via API-NNN-revM) must update this file in the same PR; otherwise `cargo test --no-run` fails.

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

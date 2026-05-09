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

**Stage 2 A0 closed**（2026-05-09，自起步 commit `bb421e2` 起共 9 笔 commit：`bb421e2` validation + workflow 起步 → `91cccad` decisions + api 双骨架 → `3f62842 / 96e3b9c / 1e57942 / 622204f / 9b7085d` 5 笔 review 修正 batch 1–5 → `452fb89` A0 闭合同步 → 本 commit batch 6 review 修正）：

A0 [决策] 落地四份文档锁定 stage-2 全部技术 / API 决策点：

- `docs/pluribus_stage2_decisions.md`（D-200..D-283，含 D-220a / D-236b / D-228 sub-stream 派生协议 + batch 6 一组 rev：D-202-rev1 / D-206-rev1 / D-211-rev1 / D-216-rev1 / D-218-rev1 / D-220a-rev1 / D-224-rev1 / D-244-rev1 / D-253-rev1）：§1 action（D-200 默认 5-action / D-201 PHM stub / D-204 `Check` 局面剔除 `Fold` / D-205 fallback 链 min_to → AllIn / D-206 fold-collapsed AllIn 状态转移 + D-206-rev1 Call/AllIn 折叠优先级 / D-209 输出顺序）+ §2 info（D-210 6 桶 position / D-211 5 桶 stack `[0,20)/[20,50)/[50,100)/[100,200)/[200,+∞) BB` + D-211-rev1 stack_bucket 来源钉到 `TableConfig::initial_stack` / D-212 5 状态 betting_state preflop+postflop 共用 / D-215 统一 64-bit `InfoSetId` layout 24+4+4+3+3+26 bit / D-217 169 hand class closed-form 公式 + 12 锚点 / D-218 canonical hole / board id + D-218-rev1 联合 (board, hole) canonical observation id / D-219 postflop 不依赖 preflop key）+ §3 equity（D-220 iter=10k / D-220a EQ-001 反对称按街分流 + D-220a-rev1 双 RngSource 协议 / D-221 `feature_set_id=1` = EHS² + OCHS(N=8) n_dims=9 / D-222 OCHS N=8 / D-223 EHS / EHS² 计算口径 / D-224 finite 校验 + D-224-rev1 invalid input 走 `Result<_, EquityError>` / D-227 EHS² rollout river 0 / turn 46 / flop 1081 / D-228 RngSource sub-stream 派生协议 SplitMix64 finalizer + op_id 表）+ §4 clustering（D-230 k-means + L2 / D-231 k-means++ + RngSource / D-232 max_iter=100 / centroid_shift_l_inf 1e-4 / D-233 `T_emd = 0.02` / D-234 1D EMD CDF 差分 / D-235 量化 SCALE=2^40 + N ≤ 2_000_000 / D-236 空 cluster 切分 / D-236b 训练完成后 cluster id 重编号 0=最弱 N-1=最强 / D-237 同 (BucketConfig, training_seed, feature_set_id) BLAKE3 byte-equal / D-238 三街独立训练 / D-239 preflop 不进 clustering）+ §5 bucket table（D-240 magic `b"PLBKT\0\0\0"` 8 字节 / D-241 centroid u8 quantized / D-244 80-byte 定长 header + 变长 body + 32-byte BLAKE3 trailer + §⑨ 三段绝对偏移表 + D-244-rev1 header `n_canonical_observation_<street>` 字段语义 + lookup_table 联合 observation 索引 / D-247 5 类错误 / D-246 v1 reader 拒绝 v2 / D-249 Python 跨语言 reader）+ §6 crate（D-250 不引 ndarray / D-251 `artifacts/` gitignore / D-252 `abstraction::map` 子模块 `clippy::float_arithmetic` 死锁 / D-253 顶层 re-export + D-253-rev1 补 `BetRatio / ConfigError / BettingState / StreetTag / EquityError` / D-255 `memmap2` 引入）+ §7 外部对照（D-260 自洽性优先 + OpenSpiel 轻量对照 / D-261 169 类成员集合相等 / D-262 ≥1 类不一致 P0 / D-263 F3 [报告] 接入）+ §8 stage 1 边界（D-270..D-276 只读继承 + 浮点边界扩展）+ §9 SLO（D-280..D-284）。
- `docs/pluribus_stage2_api.md`（API-200..API-302 + batch 6 rev 一组：AA-003-rev1 / AA-004-rev1 / IA-006-rev1 / EQ-001-rev1 / EQ-002-rev1 / BT-005-rev1 / BT-008-rev1 / EquityCalculator-rev1 / BetRatio::from_f64-rev1 / `BucketTable::lookup` 签名修订）：§1 `AbstractAction` / `AbstractActionSet` / `ActionAbstractionConfig` / `BetRatio` 整数化（含 D-202-rev1 量化协议）/ `ActionAbstraction` trait + `DefaultActionAbstraction` + AA-001..AA-008 不变量（AA-003-rev1 fallback 优先级 + AA-004-rev1 含 Call/AllIn 折叠）+ §2 `InfoSetId` 64-bit + `BettingState` 5 状态 + `StreetTag` 4 街 + `canonical_hole_id` / `canonical_observation_id` 公开 helper（D-218-rev1）+ `InfoAbstraction` trait（含 D-211-rev1 stack_bucket 来源 + IA-006-rev1 前置条件）+ `PreflopLossless169` + `PostflopBucketAbstraction` + IA-001..IA-007 不变量 + §3 `EquityCalculator` trait（4 方法返回 `Result<_, EquityError>`，含 F18 落地的 `equity_vs_hand` pairwise 接口；EQ-001-rev1 反对称双 RngSource 协议 / EQ-002-rev1 finite 仅 Ok 路径）+ `EquityError` 5 类 + `MonteCarloEquity` + EQ-001..EQ-005 不变量 + §4 `BucketTable` mmap（`lookup(street, observation_canonical_id)` 单维入参 + `n_canonical_observation` getter）+ `BucketTableError` 5 类 + BT-001..BT-008 不变量（BT-005-rev1 + BT-008-rev1 偏移表完整性 + n_canonical_observation 上界检查）+ §5 `tools/train_bucket_table.rs` CLI + §6 顶层 `lib.rs` re-export（D-253-rev1 含 `BetRatio` / `ConfigError` / `BettingState` / `StreetTag` / `EquityError` / `canonical_hole_id` / `canonical_observation_id`）+ `abstraction::cluster::rng_substream` 公开 contract（D-228）+ §7 与 stage 1 类型桥接（`InfoSetId::from_game_state` / `AbstractAction::to_concrete`）。
- `docs/pluribus_stage2_validation.md`：§1–§7 + §通过标准 + §SLO 汇总 全部 `[D-NNN 待锁]` 占位补成实数（与 decisions.md 对齐 + `prior_action` 命名 → `betting_state`）；§修订历史 首条记录 A0 闭合同步。
- `docs/pluribus_stage2_workflow.md` §修订历史 首条 §A-rev0 落地，carry forward stage-1 §B-rev1 §3 / §B-rev1 §4 / §C-rev1 / §D-rev0 §1–§3 / §F-rev1 处理政策；按时间线列出 5 笔 review batch + F12 不修说明。

A0 起步起 review 子 agent 共发现 21 处独立 spec drift（编号 F7..F27），通过 6 笔 commit 落地 20 处修正（F12 维持不修）：

| commit | batch | 修正主题 |
|---|---|---|
| `3f62842` | batch 1 | F7 / F8 / F9 / F17 — InfoSet 编码 + 类型一致性（D-215 统一 64-bit layout / `StreetTag` vs `Street` 隔离 / `BettingState` 5 状态展开 / `position_bucket` 4 bit 支持 2..=9 桌大小） |
| `96e3b9c` | batch 2 | F11 / F13 — RngSource sub-stream 派生协议（D-228 SplitMix64 finalizer + op_id 表）+ bucket table header 80-byte 偏移表（D-244 §⑨ 解决 BT-007 byte flip 变长段定位 panic） |
| `1e57942` | batch 3 | F14 — D-217 169 hand class closed-form 公式 + 12 条边界锚点表（B1 [测试] 在 [实现] 之前直接基于公式枚举断言） |
| `622204f` | batch 4 | F10 / F15 / F16 — D-206 fold-collapsed `AllIn` `betting_state` 转移澄清 / D-235 N ≤ 2_000_000 + 量化 SCALE=2^40 / D-243 schema_version vs BLAKE3 reproducibility 耦合标注（v1 only 不解决，stage 3 hook） |
| `9b7085d` | batch 5 | F18 — D-220a / EQ-001 `equity_vs_hand` pairwise 接口（API §3 trait 新增第三个方法，反对称只在 pairwise 路径成立——`equity(hole, board, rng)` random-opp 数学上不满足反对称） |
| 本 commit | batch 6 | F19 (P0) postflop bucket lookup 联合 (board, hole) canonical observation id（D-216-rev1 / D-218-rev1 / D-244-rev1 / BT-005-rev1，`BucketTable::lookup` 签名 3 入参 → 2 入参）+ F20 (P0) Call/AllIn 折叠优先级（D-206-rev1 / AA-004-rev1，all-in call 保留 AllIn）+ F21 (P1) postflop stack_bucket 来源钉到 `TableConfig::initial_stack`（D-211-rev1）+ F22 (P1) terminal `InfoSetId` 改为 caller error / panic（IA-006-rev1）+ F23 (P1) `EquityCalculator` 4 方法返回 `Result<_, EquityError>`（D-224-rev1 / EquityCalculator-rev1 / EQ-002-rev1）+ F24 (P2) AA-003-rev1 fallback 优先级显式化 + F25 (P2) preflop pairwise 反对称双 RngSource 协议（D-220a-rev1 / EQ-001-rev1）+ F26 (P2) D-253-rev1 顶层 re-export 列表补齐 `BetRatio / ConfigError / BettingState / StreetTag / EquityError` + F27 (P2) `BetRatio::from_f64` 量化协议（half-to-even / 范围 / `ConfigError::DuplicateRatio`，D-202-rev1） |

F12 维持不修（理论 P3：feature 精度 ~5e-3 远高于 d2 量化失效阈值 1e-12，工程不触发）。

A0 角色边界审计：仅修 `docs/` 下 4 份文档（`pluribus_stage2_decisions.md` 起草 + `pluribus_stage2_api.md` 起草 + `pluribus_stage2_validation.md` 占位补完 + `pluribus_stage2_workflow.md` §修订历史 首条同步）+ `CLAUDE.md` 状态翻面；`src/` / `tests/` / `benches/` / `fuzz/` / `tools/` / `proto/` **未修改一行**——A0 [决策] role 0 越界（继承 stage-1 §F-rev2 / §F-rev0 / §C-rev1 0 越界形态）。

stage-2 起步四项关键决策已贯通四份文档：

1. 默认 5-action（`fold / check / call / 0.5×pot / 1×pot / all-in`），`ActionAbstractionConfig` 1–14 raise size 配置接口预留但 stage 2 不实跑大配置（仅 smoke test "配置可加载 + 输出确定性"）。
2. Bucket lookup table 运行时 mmap 大文件（独立二进制 artifact，**stage 6 实时搜索 lookup 表也走这条路**，stage 2 落地基础设施；`artifacts/` gitignore + git LFS / release artifact 分发，**不进 git history**）。
3. Postflop 默认 `flop = 500 / turn = 500 / river = 500`（path.md ≥ 500 字面），`BucketConfig` 接口可配置每条街独立数量；stage 2 验收**只跑** 500/500/500，其它配置 smoke。
4. 决策已闭合，A1 [实现] 起步并在本 commit 落地（详见下文 **Stage 2 A1 closed**）。

**Stage 2 A1 closed**（2026-05-09，本 commit）：API 骨架代码化按 `pluribus_stage2_workflow.md` §A1 全部输出落地：

- `src/abstraction/` 完整 10 文件模块树：`mod.rs` / `action.rs` / `info.rs` / `preflop.rs` / `postflop.rs` / `equity.rs` / `feature.rs` / `cluster.rs`（含 `pub mod rng_substream`）/ `bucket_table.rs` / `map/mod.rs`。
- 全部公开类型 + trait + 方法签名严格匹配 `pluribus_stage2_api.md` API-200..API-302（含 batch 6 一组 rev：`BetRatio::from_f64-rev1` 量化协议 / `ConfigError::DuplicateRatio` 新变体 / AA-003-rev1 fallback 优先级 / AA-004-rev1 折叠优先级 / IA-006-rev1 前置条件 panic / `EquityCalculator` 4 方法 `Result<_, EquityError>` / `EquityError` 5 类 / `BucketTable::lookup` 单维 `observation_canonical_id` / `BucketTable::n_canonical_observation` / `canonical_hole_id` / `canonical_observation_id` helper / D-244-rev1 80-byte header 偏移表）。
- 全部函数体 `unimplemented!()` / `todo!()` 占位（`BucketConfig::default_500_500_500()` 与 `BetRatio::HALF_POT / FULL_POT` 等 `const` 路径直接给值，B2 测试可读但 trait 行为仍未实现）。
- `tests/api_signatures.rs` 追加 stage 2 trip-wire `_stage2_api_signature_assertions()`：覆盖 `BetRatio` 量化 + `AbstractActionSet` 全 5 方法 + `ActionAbstractionConfig` + `DefaultActionAbstraction` + `AbstractAction::to_concrete` + `InfoSetId` 6 getter + `canonical_hole_id` + `PreflopLossless169` 3 方法 + `canonical_observation_id` + `PostflopBucketAbstraction` 3 方法 + `MonteCarloEquity` 5 方法 + `BucketConfig` 2 方法 + `BucketTable` 9 方法 + `derive_substream_seed` + 全 15 个 op_id 常量；与 stage 1 同形态 `!` 返回类型 trip-wire（任一签名漂移立即在 `cargo test --no-run` 失败）。
- `Cargo.toml` 追加 `memmap2 = "0.9"`（D-255，A1 仅声明依赖；C2 落地真实 mmap 加载路径）。
- `abstraction::map/mod.rs` 顶 `#![deny(clippy::float_arithmetic)]` inner attribute（D-252，clippy 在本模块内任何 `f32` / `f64` 算术触发硬错；A1 模块体为空，B2 填充 InfoSetId bit pack/unpack helpers 时该 lint 提供 byte-equal 保护）。
- `abstraction::cluster::rng_substream` 子模块顶层暴露 `derive_substream_seed(master_seed, op_id, sub_index) -> u64` + 全 15 个 op_id 命名常量（D-228 公开 contract，签名固定 + 函数体 `unimplemented!()` 留 B2 落地 SplitMix64 finalizer 实现）；`lib.rs` 走 `pub use crate::abstraction::cluster::rng_substream;` 把整个 sub-module 顶层 re-export，便于 `tests/clustering_determinism.rs` 等 [测试] 直接 `use poker::rng_substream::*;`。
- `lib.rs` D-253-rev1 顶层 re-export 全部 21 个公开类型 / trait / helper 落地（action 7 / info 4 / preflop 2 / postflop 2 / equity 3 / bucket_table 3）：`AbstractAction / AbstractActionSet / ActionAbstraction / ActionAbstractionConfig / BetRatio / ConfigError / DefaultActionAbstraction / BettingState / InfoAbstraction / InfoSetId / StreetTag / canonical_hole_id / PreflopLossless169 / canonical_observation_id / PostflopBucketAbstraction / EquityCalculator / EquityError / MonteCarloEquity / BucketConfig / BucketTable / BucketTableError`，外加 1 个子模块 re-export `abstraction::cluster::rng_substream`（D-228 公开 contract，含 `derive_substream_seed` 函数 + 15 个 op_id 常量）。
- A1 出口标准全绿：`cargo build --all-targets` ok；`cargo clippy --all-targets -- -D warnings` ok（`clippy::float_arithmetic` 限定 `abstraction::map` 不被全局触发）；`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` ok（中文 role tag `[实现]` / `[决策]` / `[测试]` / `[报告]` 全部 `\[..\]` 转义避开 rustdoc broken-intra-doc-links 死锁）；`cargo fmt --all --check` ok；`cargo test`（默认）**104 passed / 19 ignored / 0 failed across 16 test crates**（与 stage-1 baseline 完全一致——本 commit 在既有 `#[test] api_signatures_locked` 函数内追加 `_stage2_api_signature_assertions()` 调用，未新增 `#[test]` 函数；`#[ignore]` 数与 stage-1 baseline 完全一致；A1 不引入测试回归，stage 2 抽象层骨架的 fail-on-call 行为留待 B1 [测试] 测试代码触发）。
- A1 角色边界审计：仅写 / 改 stage 1 锁定外的产品代码与配置——新建 10 文件 `src/abstraction/**`、修 `src/lib.rs` re-export、修 `Cargo.toml` 依赖、修 `tests/api_signatures.rs` 追加 stage 2 trip-wire（已在 §F1 / §F-rev0 carve-out 政策下被认定为 [测试] 文件；A1 [实现] 触发理由：`pluribus_stage2_workflow.md` §A1 §输出 列表第 3 条明确 "tests/api_signatures.rs 追加阶段 2 trait 签名编译断言"，并继承 stage-1 §B-rev1 §3 同型角色边界——签名 trip-wire 的同 commit 同步责任由 [实现] 承担，避免 B1 [测试] 起步前签名漂移可能性）。`src/core/` / `src/rules/` / `src/eval/` / `src/history/` / `src/error.rs` / `proto/` / `benches/` / `fuzz/` / `tools/` **未修改一行**——0 越界（继承 stage-1 §F-rev2 / §F-rev0 / §C-rev1 0 越界形态）。

**A1 后 review 措辞收尾 batch 7**（2026-05-09，本 commit）：A1 闭合 commit `c4107ee` 落地后 review 抽查发现 4 处文档措辞观察（O1..O4），3 处 doc-only 修正、1 处保留（`_opaque: ()` 占位为 B2 命名 struct 字段做 forward-looking 不动）。**0 spec 变化、0 公开签名变化、0 不变量变化、0 测试回归、0 角色越界**——`src/abstraction/{action,info,preflop,postflop,equity,bucket_table,cluster,feature,map/mod}.rs` 主体 + `tests/api_signatures.rs` trip-wire + `Cargo.toml` / `Cargo.lock` 依赖列表 **未修改一行**。触发文件：`docs/pluribus_stage2_api.md`（§2 trait doc + §F21 carve-out 文 + §F21 影响 ④：A1→B2 三处对齐 + §修订历史 batch 7 子节）+ `src/abstraction/mod.rs`（line 14/16 「模块私有」改写为「D-254 不在 lib.rs 顶层 re-export」）+ 本文件（line 136 / 172 计数 14 → 21 + 1 子模块 + 本段 batch 7 翻面）+ `docs/pluribus_stage2_workflow.md`（§修订历史 §A-rev1 + batch 7 子节追加）。**复跑 5 道 gate**（A1 baseline 同型）：fmt / build / clippy / doc / test 全绿，104 passed / 19 ignored / 0 failed across 16 test crates。详见 `pluribus_stage2_api.md` §修订历史 batch 7 + `pluribus_stage2_workflow.md` §A-rev1 batch 7 子节。

**Stage 2 B1 closed**（2026-05-09，commit `14508bb` 主体落地 + 本 commit B-rev0 batch 2 后置 review 6 项 + 3 处 carve-out）：核心场景测试 + harness 骨架按 `pluribus_stage2_workflow.md` §B1 §输出 全部 5 类落地（详见 `pluribus_stage2_workflow.md` §修订历史 §B-rev0 + B-rev0 batch 2）：

- **A 类 17 fixed scenario `#[test]`**（commit `14508bb`）：`tests/action_abstraction.rs` 10 + `tests/info_id_encoding.rs` 7（含 `info_abs_postflop_bucket_id_in_range` `#[ignore]`）。本 commit batch 2 在 `tests/action_abstraction.rs` 加 2 个 `action_abs_short_bb_3bet_min_to_above_stack_priority_case{1,2}`（API §F20 影响 ③ 字面要求至少 2 case）+ 重写 `action_abs_bet_pot_falls_back_to_min_raise_when_below`（用 0.001 ratio 真实驱动 AA-003-rev1 ① fallback）。
- **B 类 preflop 169 lossless `#[test]` 5 个**（commit `14508bb`，`tests/preflop_169.rs`）：2 closed-form 独立通过 + 3 stub-driven B2 driver。本 commit 0 修改。
- **C 类 equity Monte Carlo 自洽性 harness**（commit `14508bb` 9 `#[ignore]`，本 commit batch 2 +3 = 12）：本 commit 加 `equity_iter_too_low_returns_err`（IterTooLow 错误路径补完）+ `ochs_shape_finite_range_smoke`（EQ-002-rev1 OCHS shape + finite + range 不变量）+ `ehs_squared_finite_range_smoke`（EQ-002-rev1 ehs² 三街 finite + range）。`tests/equity_self_consistency.rs` doc 顶补 B-rev0 carve-out 引用段。
- **D 类 clustering determinism harness**（commit `14508bb`，`tests/clustering_determinism.rs` 3 active D-228 op_id 命名空间 + 4 `#[ignore]`）：本 commit 0 修改。
- **E 类 criterion benchmark harness**（commit `14508bb`，`benches/baseline.rs` 追加 `abstraction/info_mapping` + `abstraction/equity_monte_carlo`）：本 commit 0 修改。
- **新增 `tests/canonical_observation.rs`**（本 commit batch 2，H4）：API §1040 影响 ⑤ 字面要求 B1 [测试] 起草本文件。8 个 `#[test]`：3 街 1k repeat smoke（确定性）+ 3 街 suit-rename invariance（D-218-rev1 花色对称等价类核心不变量）+ 1 flop compactness smoke（id 紧凑性）+ 1 preflop should_panic（前置条件）。workflow §B1 §输出 A 类列表与 API §1040 字面要求 doc drift 走 carve-out 消解（详见 workflow §B-rev0 batch 2 carve-out #3）。
- **3 处 carve-out**（本 commit batch 2）：(1) C 类 equity 12 个 `#[ignore]` 由 B2 [实现] 闭合 commit 同 commit 取消（[测试] 角色越界由 §B-rev1 §3 同型 carve-out 追认，B1 §出口 line 250 与 [实现] "禁修测试" 规则在 A1 全 `unimplemented!()` 状态下硬冲突）；(2) `info_abs_postflop_bucket_id_in_range` `#[ignore]` 文案显式指向 B2 [决策] 缺位——B2 [实现] 必须三选一暴露 `BucketTable::stub_for_postflop` / `tools/build_minimal_bucket_table.rs` / `PostflopBucketAbstraction::new_with_table_in_memory` 中之一作为 stub 构造路径；(3) API §1040 影响 ⑤ vs workflow §B1 §输出 doc drift 消解政策——API 字面优先（"硬"约束 vs"软"路线），按 API 落地 `tests/canonical_observation.rs`，workflow §B1 §输出 不追加新条目。

B1 + B-rev0 batch 2 出口数据（commit 落地实测；本机 1-CPU AMD64 debug profile）：

- `cargo build --all-targets` ok / `cargo fmt --all --check` ok / `cargo clippy --all-targets -- -D warnings` ok / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` ok。
- `cargo test --no-fail-fast`（默认）：110 passed / 29 failed / 36 ignored across 22 test crates。其中：
    - **stage-1 baseline 16 crates 维持** `104 passed / 19 ignored / 0 failed`，与 stage1-v1.0 tag byte-equal（D-272 不退化要求满足）。
    - **stage-2 B1 + batch 2 新 6 crates** `6 passed / 29 failed / 17 ignored`：A 类 action_abstraction 12 panic + info_id_encoding 7 panic + 1 ignored + canonical_observation 7 panic + 1 should_panic 通过 + B 类 preflop_169 2 closed-form 独立通过 + 3 stub-driven panic + C/D 类 17 ignored（equity 12 + clustering 4 + postflop bucket 1）+ 3 const-only clustering active 通过。整体 §B1 §出口 line 248–250 字面预期满足。

B1 角色边界审计（含 batch 2）：仅触 `tests/`（commit `14508bb` 新 5 + batch 2 新 1 + batch 2 修 3 = 9 文件，部分重叠）+ `benches/baseline.rs`（commit `14508bb` 追加 2 group，本 commit 0 修改）+ `docs/pluribus_stage2_workflow.md` §B-rev0 + §B-rev0 batch 2 + `CLAUDE.md`，`src/` / `Cargo.toml` / `Cargo.lock` / `fuzz/` / `tools/` / `proto/` **未修改一行**——B1 [测试] 0 越界（继承 stage-1 §B-rev1 §3 / §C-rev1 / §D-rev0 0 越界形态）。`tests/scenarios_extended.rs` **未触动**（API §F20 影响 ② 显式 carry-forward 到 C1 §C1 line 317 字面落地）。

**下一步：stage 2 B2 [实现]** — 让 B1 全绿，按 §B2 §输出 列表落地 `DefaultActionAbstraction` / `PreflopLossless169` / `PostflopBucketAbstraction` 占位实现 / `MonteCarloEquity` 朴素实现 / `EHSCalculator` 朴素实现。B2 出口 `cargo test`（默认）必须把本 §B-rev0 batch 2 实测 110 passed → 至少 ≥ 130 passed（含 12 条 C 类 equity `#[ignore]` 取消转 active + 1 条 `info_abs_postflop_bucket_id_in_range` 取消 ignore），并验证 §B2 §出口 line 281 "equity Monte Carlo 反对称误差在 D-220 容差内"。具体计数留 B2 [实现] 实测核对。B2 [实现] agent **只写产品代码，不修测试**（继承 stage-1 §B2 / §C2 角色边界）；本 commit 三处 carve-out 在 B2 闭合时同步落地。

## Documents and their authority

The stage-1 docs form a contract hierarchy (frozen as of `stage1-v1.0`). Read them in this order before making stage-1 / stage-2 changes:

1. `docs/pluribus_path.md` — overall 8-stage roadmap, stage acceptance gates, hardware/time budgets. Stages 4–6 thresholds are deliberately stricter than the original Pluribus path; do **not** weaken them.
2. `docs/pluribus_stage1_validation.md` — quantitative pass criteria for stage 1 (e.g. 1M-hand fuzz, ≥10M eval/s, 100k-hand cross-validation with PokerKit).
3. `docs/pluribus_stage1_decisions.md` — locked technical/rule decisions (D-001 … D-103). **Authoritative spec for implementers.**
4. `docs/pluribus_stage1_api.md` — locked Rust API contract (API-NNN). **Authoritative spec for testers.**

Stage-2 docs (A0 closed as of 2026-05-09)：

5. `docs/pluribus_stage2_validation.md` — quantitative pass criteria for stage 2 (preflop 169 lossless 100% / postflop bucket EHS std dev < 0.05 / clustering determinism / abstraction mapping ≥100k mapping/s / mmap bucket table schema). All `[D-NNN 待锁]` placeholders filled by A0 closure (与 decisions.md `D-200..D-283` 锁定数值同步)。
6. `docs/pluribus_stage2_workflow.md` — 13-step test-first workflow for stage 2, mirrors `pluribus_stage1_workflow.md`. §修订历史 §A-rev0 首条已落地（A0 闭合同步 + carry forward stage-1 处理政策）。
7. `docs/pluribus_stage2_decisions.md` — A0 [决策] 输出，**locked**。D-200 起编号（与 stage-1 D-NNN 不冲突）；含 D-200..D-283 + D-220a / D-236b / D-228 sub-stream 派生协议 + batch 6 一组 rev：D-202-rev1 / D-206-rev1 / D-211-rev1 / D-216-rev1 / D-218-rev1 / D-220a-rev1 / D-224-rev1 / D-244-rev1 / D-253-rev1。**Authoritative spec for implementers.**
8. `docs/pluribus_stage2_api.md` — A0 [决策] 输出，**locked**。API-200..API-302（含 F18 落地的 `EquityCalculator::equity_vs_hand` pairwise 接口 + batch 6 一组 rev：AA-003-rev1 / AA-004-rev1 / IA-006-rev1 / EQ-001-rev1 / EQ-002-rev1 / BT-005-rev1 / BT-008-rev1 / EquityCalculator-rev1 / BetRatio::from_f64-rev1 / `BucketTable::lookup` 签名 3 → 2 入参 / `EquityError` 新增 / `canonical_hole_id` + `canonical_observation_id` 新增 helper）。**Authoritative spec for stage-2 testers.**

If a change affects a decision or API signature, you must follow the **D-100 / API-NNN-revM** amendment flow described in `pluribus_stage1_decisions.md` §10 and `pluribus_stage1_api.md` §11 — append a `D-NNN-revM` / `API-NNN-revM` entry, never delete the original, and bump `HandHistory.schema_version` if serialization is affected. Both docs have a "修订历史" subsection. Past rev entries:

- **D-033-rev1** (decisions §10) — pin "incomplete raise 不重开 raise option" to TDA Rule 41 / PokerKit-aligned semantics: per-player `raise_option_open: bool`, full raise opens for un-acted players + closes raiser, call/fold closes self only, incomplete touches no flags. Drives `tests/scenarios.rs` #3 (already-acted SB → `raise_range = None`) vs #4 (still-open BTN → `raise_range = Some(min_to=650)`). validation.md §1 第 22 行措辞同步收紧。
- **D-039-rev1** (decisions §10) — odd-chip 余 chip 由「逐 1 chip 沿按钮左侧分配」改为「**整笔给按钮左侧最近的获胜者**」，对齐 PokerKit 0.4.14 默认 chips-pushing divmod 语义。每个 pot 仍独立计算；不同 pot 之间互不影响。`payouts()` 行为变化但公开签名不变；`HandHistory.schema_version` 不 bump（序列化格式未动）；`pluribus_stage1_validation.md` §3 同步。该 rev 在 B2 cross-validation 100 手 vs PokerKit 出现 1-chip 分歧后落地，遵循 workflow §B2 「默认假设我方理解错了规则」原则。
- **API-001-rev1** (api §11) — `HandHistory::replay` / `replay_to` return `Result<_, HistoryError>` instead of `RuleError`; `HistoryError::Rule { index, source: RuleError }` wraps the underlying rule error.

## Workflow (multi-agent, strict role boundaries) — applies to all stages

Each stage is organized as `A → B → C → D → E → F` (13 steps). Stage-1 workflow lives in `docs/pluribus_stage1_workflow.md`; stage-2 workflow lives in `docs/pluribus_stage2_workflow.md` (mirror structure). Every step is tagged `[决策] / [测试] / [实现] / [报告]` and **role boundaries are enforced**:

- `[测试]` agent writes tests / harness / benchmarks only. **Never modify product code.** If a test reveals a bug, file an issue for `[实现]` to fix.
- `[实现]` agent writes product code only. **Never modify tests.** If a test fails, fix the product code; only edit the test if it has an obvious bug, and only after review.
- `[决策]` and `[报告]` produce or modify docs in `docs/`.

When the user asks you to do stage work, identify which stage and which step (A0 / A1 / B1 / …) the task belongs to and operate within that role. **Stage 1 is closed**（F3 [报告] is done）：所有 13 步按 workflow §修订历史 时间线闭合。**Stage 2 A0 closed**（2026-05-09）：四份 stage-2 文档（validation / workflow / decisions / api）全部锁定 + batch 6 review 修正全部落地。**Stage 2 A1 closed**（2026-05-09，commit `c4107ee` 主体落地 + batch 7 后置 review 措辞收尾 0 spec / 0 签名 / 0 测试回归）：API 骨架代码化按 §A1 输出列表全部落地（详见上方 §Stage 2 A1 closed 段落）。**Stage 2 B1 closed**（2026-05-09，本 commit）：核心场景测试 + harness 骨架按 §B1 §输出 全部 5 类落地——A 类 17 fixed scenario `#[test]`（`tests/action_abstraction.rs` 10 + `tests/info_id_encoding.rs` 7 + `info_abs_postflop_bucket_id_in_range` `#[ignore]`）+ B 类 `tests/preflop_169.rs` 5 测试（2 closed-form 独立通过 + 3 stub-driven B2 driver）+ C 类 `tests/equity_self_consistency.rs` 9 `#[ignore]` harness（EQ-001-rev1 反对称按街分流 + EHS 单调性 + EQ-005 deterministic + 错误路径）+ D 类 `tests/clustering_determinism.rs` 3 active D-228 op_id 命名空间断言 + 4 `#[ignore]` 骨架（SplitMix64 / 区分性 / clustering BLAKE3 / 跨线程）+ E 类 `benches/baseline.rs` 追加 `abstraction/info_mapping` + `abstraction/equity_monte_carlo` 两 group（D-259 命名前缀 `abstraction/*`，与 stage-1 5 条 bench 共存）。出口数据见上方 §B1 closed 段落（cargo build / fmt / clippy / doc 全绿；`cargo test --no-fail-fast` 109 passed / 20 failed / 33 ignored across 21 test crates，stage-1 baseline 16 crates **维持** 104 passed / 19 ignored / 0 failed byte-equal，与 D-272 不退化要求一致；stage-2 新 5 crates 5 passed / 20 failed / 14 ignored 与 §B1 §出口 line 248–250 字面预期完全一致：A 类 panic on unimplemented + B 类 closed-form 独立通过 + C/D 类 harness ignored 不 panic）。本 commit 仅触 `tests/`（5 新文件）+ `benches/baseline.rs`（追加 2 group）+ `docs/pluribus_stage2_workflow.md` §B-rev0 + `CLAUDE.md`，`src/` / `Cargo.toml` / `Cargo.lock` / `fuzz/` / `tools/` / `proto/` **未修改一行**——B1 [测试] 0 越界（继承 stage-1 §B-rev1 §3 / §C-rev1 / §D-rev0 0 越界形态）。详见 `pluribus_stage2_workflow.md` §修订历史 §B-rev0。下一步：B2 [实现]（让 B1 全绿，按 §B2 §输出 列表落地 `DefaultActionAbstraction` / `PreflopLossless169` / `PostflopBucketAbstraction` 占位 / `MonteCarloEquity` / `EHSCalculator` 朴素实现）。stage-2 §修订历史 首条已显式 carry forward stage-1 处理政策（§B-rev1 §3 越界追认 / §B-rev1 §4 CLAUDE.md 同步责任 / §C-rev1 零产品代码改动也需 closure / §D-rev0 §1–§3 D-NNN-revM 翻语义评估测试反弹 / §F-rev1 错误前移单点不变量）。历史 stage-1 关键边界事件：(1) B2 closure crossed the [实现]→[测试] boundary by completing two test files that B1 had deliberately left as stubs — see workflow §修订历史 B-rev1; (2) C2 closure carved out 「规则引擎 100k cross-validation 测试」 留给 [测试] agent — see §C-rev1; (3) D1 [测试] 实跑 100k cross-validation 暴露 105 条产品代码分歧 — see §C-rev2; (4) D2 [实现] 修完 105 条分歧 + 同 commit 翻新 2 条 scenario 测试到 D-037-rev1 语义；越界以 carve-out 追认 — see §D-rev0; (5) E1 [测试] 0 越界落地 bench harness + SLO 断言；多核多线程 SLO 在 1-CPU host 走 skip-with-log 留给多核 follow-up — see §E-rev0; (6) E2 [实现] 0 越界把 5/5 SLO 断言全部转绿（多线程在 1-CPU host 走 skip-with-log 不变）；apply 路径去 clone + 评估器换 bitmask 顺带让 1M fuzz / 1M determinism / 1M three-piece evaluator 等正确性测试加速 5–24× — see §E-rev1; (7) F1 [测试] 0 越界落地 schema 兼容 / corrupted history / evaluator lookup 三件套；评估器 lookup-table 加载失败路径在 E2 const-baked 设计下结构性缺位，F1 用结构性断言 + 黑盒完备性扫描间接覆盖；4 条域违规走 `#[ignore]` 留给 F2 自由 trade-off — see §F-rev0; (8) F2 [实现] 0 越界 trade-off 选择 「错误前移到 from_proto」，仅触 `src/history.rs` 一文件加 5 处域校验，4/4 F1→F2 carry-over `--ignored` 全部翻绿，「decode 后 seat 全部 < n_seats」 成单点不变量 — see §F-rev1; (9) F3 [报告] 0 越界落地 `docs/pluribus_stage1_report.md` + git tag `stage1-v1.0`；阶段 1 出口检查清单可在单核 host 落地的项目全部归零，剩余 3 项 carve-out 与代码合并解耦 — see §F-rev2.

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

# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository status

8-stage Pluribus-style 6-max NLHE poker AI。**Stage 1 closed**（git tag `stage1-v1.0`，验收报告 `docs/pluribus_stage1_report.md`）；**Stage 2 closed**（git tag `stage2-v1.0`，验收报告 `docs/pluribus_stage2_report.md`，A0..F3 全 13 步 closed）。**Stage 3 起步 batch 1 §G-batch1 §1 [决策]** + **§G-batch1 §2 [测试]** + **§G-batch1 §3.1 [实现]** closed 2026-05-11（D-218-rev2 决策 + 5 条契约测试 + canonical_enum.rs Waugh-style hand isomorphism + N 实测修正 stage 2 §C-rev1 §2 ~50x 估算误差）；下一步 §G-batch1 §3.2 [实现]（postflop.rs 接入 canonical_enum；详见下文 §下一步：Stage 3 起步 batch 1）。

历史 batch 出口数据（stage 1 的 B/C/D/E/F 各步、stage 2 的 A0 batch 1–6 review / A1 batch 7 / B1 batch 2）不在本文件保留——查阅顺序：

1. `docs/pluribus_stage1_report.md` — stage-1 验收报告，含 F3 全套出口数据 + 9 条 §修订历史 carve-out 索引。
2. `docs/pluribus_stage2_report.md` — stage-2 验收报告，含 F3 全套出口数据 + 4 项 carve-out 现状索引（D-218-rev2 / D-282 host-load / 跨架构 1M / 24h fuzz）。
3. `docs/pluribus_stage1_workflow.md` §修订历史（B-rev1 / C-rev1 / C-rev2 / D-rev0 / E-rev0 / E-rev1 / F-rev0 / F-rev1 / F-rev2）= stage-1 9 条 carve-out 全文。
4. `docs/pluribus_stage2_workflow.md` §修订历史（A-rev0 / A-rev1 / B-rev0 / B-rev1 / C-rev0 / C-rev1 / C-rev2 / D-rev0 / D-rev1 / E-rev0 / E-rev1 / F-rev0 / F-rev1 / F-rev2）= stage-2 12 条 carve-out 全文。
5. `git log --oneline stage1-v1.0..stage2-v1.0` — stage-2 实施提交时间线。

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
- `cargo bench --bench baseline` — stage-1 5 条 bench（eval7_naive single/batch / simulate / history encode/decode）+ stage-2 追加 3 条（`abstraction/info_mapping` / `abstraction/equity_monte_carlo` / E1 落地的 `abstraction/bucket_lookup` 3 街分流）。CI 短路径走 `--warm-up-time 1 --measurement-time 1 --sample-size 10 --noplot`，nightly 跑全量（`.github/workflows/{ci,nightly}.yml`）。
- `cargo test --release --test perf_slo -- --ignored` — 5 条 stage-1 SLO 断言 + 3 条 E1 落地的 stage-2 SLO 断言（`stage2_abstraction_mapping_throughput_*` / `stage2_bucket_lookup_p95_latency_*` / `stage2_equity_monte_carlo_throughput_*`）。

### 装 PokerKit（C2 实测可用）

```bash
uv venv --python 3.11 .venv-pokerkit
uv pip install --python .venv-pokerkit/bin/python "pokerkit==0.4.14"
PATH=".venv-pokerkit/bin:$PATH" cargo test                        # 默认 + active cross-validation
PATH=".venv-pokerkit/bin:$PATH" cargo test --release -- --ignored # full-volume
```

`.venv-pokerkit/` 已 gitignore。

## Stage 2 progress

已闭合步骤一行索引（详细出口数据、carve-out 全文、实测数字均在 `docs/pluribus_stage2_workflow.md` §修订历史 + `git show <commit>`）：

- **A0 [决策]** closed 2026-05-09，9 commits `bb421e2..452fb89` — D-200..D-283 + API-200..API-302 + 四份 stage-2 文档落地 + batch 6 一组 D/API-NNN-rev1。四项关键决策：默认 5-action / bucket table mmap 大文件（不进 git history） / postflop `flop=turn=river=500` / preflop 169 lossless + postflop k-means + L2。
- **A1 [实现]** closed 2026-05-09，commit `c4107ee`（+ 后置 `98bc952` / `1db29a7`）— `src/abstraction/` 10 文件模块树骨架；公开签名严格匹配 API；函数体 `unimplemented!()` 占位；`tests/api_signatures.rs` trip-wire 落地；`memmap2 = "0.9"` 依赖加入。
- **B1 [测试]** closed 2026-05-09，commit `14508bb`（+ `3b14d35`）— 5 类 harness 落地：action_abstraction 12 + info_id_encoding 8 + preflop_169 5 + equity_self_consistency 12 `#[ignore]` + clustering_determinism 3 active + canonical_observation 8 + 2 bench group。
- **B2 [实现]** closed 2026-05-09，commit `457be85` — 5-action / preflop 169 closed-form / postflop FNV stub / MonteCarloEquity 朴素实现 / `derive_substream_seed` 落地。同 PR 触发 stage 1 **API-004-rev1**（`GameState::config()` 只读 getter）。4 处 [测试] 角色越界 carve-out（§B-rev1）。
- **C1 [测试]** closed 2026-05-09，commit `5d6c8d6`（+ `ea1ea25`）— bucket_quality 20 / equity_features 10 / scenarios_extended sweep（≥ 380 cases）/ `tools/bucket_quality_report.py`；12 条质量门槛断言 `#[ignore]` 留 C2。
- **C2 [实现]** closed 2026-05-09，commit `2418a10` + 6 笔 §C-rev2 batch（`2718f69` / `9c0233c` / `3644b92` / `71275e1` / `1986baf` / `37b6617`）— k-means + EMD + bucket table mmap + train CLI；artifact `artifacts/bucket_table_default_500_500_500_seed_cafebabe.bin`（95 KB / gitignore）；3 处 carve-out（§C-rev1 §1 EHS² ≈ equity² / §C-rev1 §2 FNV hash canonical id 限制致 12 条质量门槛断言永留 ignore / `memmap2` `unsafe` 与 D-275 冲突走 `std::fs::read`）。issue #3 推迟 D1。
- **D1 [测试]** closed 2026-05-10，commit `e7071e0` — `fuzz/fuzz_targets/abstraction_smoke.rs` + `tests/abstraction_fuzz.rs`（3 组 6 test）+ `tests/clustering_cross_host.rs` + CI/nightly fuzz target 扩到 3 个；issue #3 baseline `tests/data/bucket-table-arch-hashes-linux-x86_64.txt`（32-seed × 3 街 × 10/10/10 × 50 iter，~107 min release）同 PR 闭合。**D1 暴露 issue #8** — `map_off_tree` `unimplemented!()` 移交 D2。
- **D2 [实现]** closed 2026-05-10，commit `e2fa74f` — `src/abstraction/action.rs::map_off_tree` D-201 PHM stub 占位实现（确定性映射 ① ≥ cap → AllIn ② ≤ max_committed → Call ③ 否则在 `config().raise_pot_ratios` 中找距离最近的 ratio；stage 6c 才完整数值正确）；issue #8 闭合。§D-rev1 §1 [实现] → [测试] 越界 carve-out（取消 2 条 fuzz ignore）+ §D-rev1 §2 cross_arch baseline 实跑 0 diverge（54.18 min release on 1-CPU host）闭合 §D-rev0 §4。
- **E1 [测试]** closed 2026-05-10，commit `c8d7ccb` — `tests/perf_slo.rs::stage2_*` 3 条 SLO 断言（D-280/281/282）+ `benches/baseline.rs abstraction/bucket_lookup` 3 街分流 bench group。E1 实测 SLO #1/#2 大幅过 / SLO #3（equity MC）~2× short，移交 E2。
- **E2 [实现]** closed 2026-05-10..2026-05-11，commit `d21c5d9`（+ `58aa951` procedural follow-through + `5177639` §E-rev1 §5 carve-out closure）— `src/abstraction/equity.rs` hot path 重写（const-generic 4 街分流 + hero-rank `[HandRank; 52*52]` precompute 让每 iter eval 2→1） + `src/eval.rs eval7` `#[inline(always)]` + `src/core/mod.rs Card::from_u8_assume_valid` + `src/core/rng.rs RngSource::fill_u64s` additive。stage 1 **API-005-rev1**（`RngSource::fill_u64s`）由 `58aa951` 追认。D-282 SLO 在 vultr 4-core idle box 50-run aggregate `mean 1102.1 / 50/50 PASS`，§E-rev1 §5 carve-out closed 2026-05-11。
- **F1 [测试]** closed 2026-05-11，commit `d23f7aa` — 4 类测试落地：`tests/bucket_table_schema_compat.rs`（9 active：常量锁定 + v1 round-trip + v2/v0/u32::MAX/feature_set_id 拒绝路径）/ `tests/bucket_table_corruption.rs`（12 active + 1 `#[ignore]`：5 类 BucketTableError 命名 case + 1k smoke byte flip + 100k full + exhaustive variant trip-wire）/ `tests/off_tree_action_boundary.rs`（11 active + 1 `#[ignore]`：5 类边界 `real_to` 命名 + multi-stage sweep + 9-value table + 1k random smoke + 1M full + overflow carve-out）/ `tests/equity_calculator_lookup.rs`（16 active + 1 `#[ignore]`：iter=0/1/u32::MAX × 4 方法 + EquityError 5 variant exhaustive + InvalidBoardLen / OverlapHole / OverlapBoard 边界）。release 4-crate 实测：48 passed / 3 ignored / 0 failed。**§F1 §出口字面 "部分会失败留给 F2" 不触发** — 5 类 BucketTableError variants 在 C2 已完整、D-201 PHM stub 在 D2 已确定性化、EquityError IterTooLow 自 B2 起就在。
- **F2 [实现]** closed 2026-05-11，commit `75a018f` — 走 stage-1 §C-rev1 / stage-2 §F-rev0 §2 字面预测形态 0 产品代码改动 carve-out closure；F1 测试 48/3/0 已被 C2/D2/B2 既有产品代码全部满足，无新边界 bug 暴露。同 commit 修 artifact BLAKE3 doc drift（§F-rev1 §2：CLAUDE.md OCHS commit `3644b92` 时手工录入的 `0a1b95e958b3...` 无 test guard，重训 ground truth body hash `4b42bf70e50c...` 替换 + 重训 artifact 覆写 stale `b2e3545...`）+ vultr 4-core EPYC-Rome idle box D-282 SLO 50-run aggregate 兜底 `mean 1093.2 / std 17.1 / min 1031.9 / max 1114.5 / 50/50 PASS`（与 §E-rev1 §5 closure 同型）。
- **F3 [报告]** closed 2026-05-11，本 commit + git tag `stage2-v1.0` — `docs/pluribus_stage2_report.md` 11 节 ~330 行验收报告 + `docs/pluribus_stage2_bucket_quality.md` 4 dim × 3 街直方图 + `docs/pluribus_stage2_external_compare.md` + `.json` preflop 169 类对照 + `tools/bucket_table_reader.py`（D-249 跨语言 reader）+ `tools/external_compare.py`（D-263 sanity 脚本）+ `tools/bucket_quality_dump.rs` binary（F3 一次性 instrumentation；与 train_bucket_table.rs tools/ 平行）+ `Cargo.toml [[bin]]` 条目 + `tools/bucket_quality_report.py` 维护补丁。1 类受控越界（tools/ 一次性接入；与 D-263 字面授权 「F3 [报告] 起草时由报告者一次性接入对照 sanity 脚本」 同形态扩展，0 src/tests 改动）。stage 2 闭合时 4 项 carve-out（D-218-rev2 真等价类 stage 3+ / D-282 host-load / 跨架构 1M aspirational / 24h fuzz self-hosted runner）全部不阻塞 stage 3 起步。

### Stage 2 当前测试基线（F3 [报告] 闭合后 / stage 2 closed）

- `cargo test --release --no-fail-fast`：**282 passed / 0 failed / 45 ignored across 35 result sections**（31 integration crates + 1 lib unit + 2 binary unit + 1 doc-test；F3 0 src/tests 改动 → integration 测试结构与 F2 closure commit `75a018f` 同型，binary unit section count +2 来自 F3 加 bucket_quality_dump binary）。stage-1 baseline 16 integration crates 维持 `104/19/0` 与 `stage1-v1.0` tag byte-equal（D-272 不退化满足）。release 全套 ~30 min（C2 bucket-table 训练 fixture × 4 大头 250-775 s/each：`clustering_determinism` / `bucket_quality` / `bucket_table_corruption` / `bucket_table_schema_compat` 顺序运行）。
- `cargo test --release --test perf_slo -- --ignored --nocapture stage2_`：**D-280 24,952,717 mapping/s（249× 余量）/ D-281 P95 153 ns（~65× 余量）/ D-282 vultr 50-run mean 1093.2 hand/s 50/50 PASS**（主 host 1-CPU + claude background 单跑 D-282 911.9 hand/s host-load borderline，§E-rev1 §5 / §F-rev1 §2 / §F-rev2 §2 carve-out）。
- `cargo bench --bench baseline ... abstraction/equity_monte_carlo/flop_10k_iter`：thrpt 中位 **916 elem/s**（不变）。
- `cargo test --release --test abstraction_fuzz -- --ignored`：1M iter 3 个 full 套件 0 panic / 0 invariant violation（不变）。
- artifact `artifacts/bucket_table_default_500_500_500_seed_cafebabe.bin`（95 KB / 不进 git history）body hash `4b42bf70e50cd3273687c2f46cb4e56271649a5df8e889ffe700f7f36c0b93a1`（CLI `content_hash`）/ whole-file b3sum `a35220bb5265c4fa8ef037626ab16cae9f97877c9c4e3b059fc5d1e074a4cc70`。跨架构 baseline `tests/data/bucket-table-arch-hashes-linux-x86_64.txt`（32-seed bucket table content_hash）byte-equal 维持；darwin-aarch64 baseline 仍 aspirational（D-052）。
- F3 一次性 dump 产物：`artifacts/bucket_quality_default_500_500_500_seed_cafebabe.json`（40 KB / gitignore）+ `docs/pluribus_stage2_bucket_quality.md` 4 dim × 3 街直方图（reflects C2 hash-based canonical_observation_id carve-out：flop 15/500 unused / turn 3/500 / river 2/500 / std_dev 通过率 5.8% / 3.4% / 2.8% / EMD 通过率 93.8% / 97.6% / 98.0% / monotonicity 244 / 251 / 231 violations，全部预期未达 path.md / D-233 字面阈值 → §C-rev1 §2 carve-out → stage 3+ D-218-rev2 真等价类落地后转 ✓）。
- External compare：`docs/pluribus_stage2_external_compare.md` + `.json` preflop 169 类成员 13/78/78 byte-equal + Rust D-217 closed-form artifact round-trip partition 6×4×12 uniform → D-262 P0 阻塞条件**不触发**。
- `cargo fmt --all --check` / `cargo build --all-targets` / `cargo clippy --all-targets -- -D warnings` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`：全绿。`tests/api_signatures.rs` byte-equal，stage 2 公开 API 0 签名漂移。

### 下一步：Stage 3 起步 batch 1 §G-batch1 §3 [实现]（D-218-rev2 真等价类枚举）

**Stage 3 起步 batch 1 §G-batch1 §1 [决策]** closed 2026-05-11（commit `6b52fbe`）— `docs/pluribus_stage2_decisions.md` §10 修订历史末尾追加 **D-218-rev2 + D-244-rev2** 真等价类枚举决策 entry：算法 Waugh 2013-style hand isomorphism + colex ranking 3 街全枚举 / N 实测 **1,286,792 + 13,960,050 + 123,156,254**（§G-batch1 §3.1 实测修正 stage 2 §C-rev1 §2 "~25K" 估算误差 ~50x）/ `canonical_observation_id` 签名 byte-equal 不变 / lookup table ~528 MB / `BUCKET_TABLE_SCHEMA_VERSION` bump 1 → 2 / k-means 推荐 mini-batch fallback / 训练时长 ≤ 120 min release / artifact 走 GitHub Release + BLAKE3 verify / 12 条 `tests/bucket_quality.rs` `#[ignore]` 转 active 路径锁 / D-275 unsafe_code carve-out 复审 3 选项（A `std::fs::read` 默认 / B mmap 解禁走 stage 1 D-275-rev1 / C sharded artifact）。

**Stage 3 起步 batch 1 §G-batch1 §2 [测试]** closed 2026-05-11（commit `14a668b`）— `tests/canonical_observation.rs` 节 6 新增 5 条 `#[ignore]` 测试钉 D-218-rev2 契约：N 常量精确值（**1,286,792 / 13,960,050 / 123,156,254**，§G-batch1 §3.1 实测修正）/ 100K 随机 uniqueness 3 街（distinct > 95K / 99.5K / 99.9K + max_id 接近 N - 1）/ flop 26M 全枚举精确 N_FLOP distinct（双重 ignore release + --ignored）；`tests/bucket_quality.rs` 12 条同型 `#[ignore]` reason 字符串 in-place 转向 `§G-batch1 §3`（断言 body 0 改动，行为 byte-equal）。`tests/canonical_observation`：12 passed / 0 failed / **5 ignored**（5 新 ignored 全部来自节 6）。`cargo build --tests` / `cargo fmt --all --check` / `cargo clippy --tests -- -D warnings` 全绿。`src/` / `tools/` / `benches/` / `fuzz/` / `Cargo.toml` / `decisions/api/validation.md` 0 改动；[测试] 角色 0 越界。

**Stage 3 起步 batch 1 §G-batch1 §3.1 [实现]** closed 2026-05-11（本 commit）— `src/abstraction/canonical_enum.rs` 新模块 ~720 行（Waugh 2013 suit canonicalize + colex 递归枚举 + lazy `Vec<u128>` sorted table + binary search lookup）+ `src/abstraction/mod.rs` 加 `pub mod canonical_enum`。**关键发现**：§G-batch1 §1 [决策] entry 中 N 值有误（stage 2 §C-rev1 §2 `13³ / 4! ≈ 91` back-of-envelope 估算被错填到 N_FLOP=25,989，实际 N_FLOP = **1,286,792**，误差 ~50x）。§G-batch1 §3.1 同 commit 修正所有 docs N 值（decisions.md / workflow.md / CLAUDE.md / tests/canonical_observation.rs 阈值）；artifact ~475 MB → **528 MB**、训练时长 60 min → **120 min release**（仍 < GitHub Release 2 GB 单文件上限，分发渠道不变）。10 条新 unit test 全绿（7 active + 3 release/--ignored，release 实测 6.8 s）；阶段 1 baseline 不退化（仅 abstraction/canonical_enum.rs 新增 + mod.rs 1 行）。

**下一步：Stage 3 起步 batch 1 §G-batch1 §3.2 [实现]**（按 `pluribus_stage2_workflow.md` §G-batch1 §3.2 字面）：`src/abstraction/postflop.rs::canonical_observation_id` 重写为 forward 调用 `canonical_enum::canonical_observation_id` + 三 `N_CANONICAL_OBSERVATION_*` 常量 re-export 自 canonical_enum。验证 `tests/canonical_observation.rs` 5 ignore 可转 active（N 常量 + 100K uniqueness × 3 街 + flop full enum），`tests/bucket_quality.rs` 12 ignore 仍指向 §G-batch1 §3.3+。**Stage 3 起步 batch 1 子步**：§3.2 (postflop 替换) → §3.3 (bucket_table schema bump) → §3.4 (tools 适配) → §3.5 (artifact 重训上传) → §3.6 (跨架构 baseline 重生) → §3.7 (D-275 选项 A/B/C 取舍) → §3.8 (12 bucket_quality ignore 取消) → §4 [报告]（CLAUDE.md ground truth hash 漂移 + stage 2 report §8 carve-out 状态翻面）。

stage 3 [决策]（D-300..D-3xx 锁 MCCFR 小规模验证决策表 + API-300.. 锁 API）排在 D-218-rev2 §G-batch1 全 [报告] closed 之后启动；按 `pluribus_path.md` §阶段 3 字面（Kuhn 0.01 exploitability / Leduc 稳定曲线 / 简化 NLHE 100M update / regret matching < 1e-9 / checkpoint round-trip）。MCCFR 变体已 user-decided：**Vanilla CFR (Kuhn/Leduc) + ES-MCCFR (简化 NLHE) 双轨**。其它两项 stage 3 候选 ((2) MCCFR 小规模 self-play / (3) blueprint host 选型 + 跨架构 baseline 实跑解 §F-rev2 §4 第 3 条 carve-out) 排 §G-batch1 后启动。

stage 2 输出的稳定 API surface（详见 `pluribus_stage2_api.md` + 报告 §11 切换说明）：`DefaultActionAbstraction` / `PreflopLossless169` / `PostflopBucketAbstraction` / `MonteCarloEquity` / `BucketTable` + `BucketTableError` / `InfoSetId` (64-bit) + `BettingState` + `StreetTag` + `InfoAbstraction` trait / `cluster::rng_substream::*` (sub-stream op_id 表 + `derive_substream_seed` D-228)。stage 1 + stage 2 不变量与反模式继续约束 stage 3。

## Documents and their authority

The stage-1 docs form a contract hierarchy (frozen as of `stage1-v1.0`). Read them in this order before making stage-1 / stage-2 changes:

1. `docs/pluribus_path.md` — overall 8-stage roadmap, stage acceptance gates, hardware/time budgets. Stages 4–6 thresholds are deliberately stricter than the original Pluribus path; do **not** weaken them.
2. `docs/pluribus_stage1_validation.md` — quantitative pass criteria for stage 1 (e.g. 1M-hand fuzz, ≥10M eval/s, 100k-hand cross-validation with PokerKit).
3. `docs/pluribus_stage1_decisions.md` — locked technical/rule decisions (D-001 … D-103). **Authoritative spec for implementers.**
4. `docs/pluribus_stage1_api.md` — locked Rust API contract (API-NNN). **Authoritative spec for testers.**

Stage-2 docs（locked as of A0 closure 2026-05-09）：

5. `docs/pluribus_stage2_validation.md` — quantitative pass criteria for stage 2（preflop 169 lossless 100% / postflop bucket EHS std dev < 0.05 / clustering determinism / abstraction mapping ≥100k mapping/s / mmap bucket table schema）。
6. `docs/pluribus_stage2_workflow.md` — 13-step test-first workflow（mirror `pluribus_stage1_workflow.md`）。§修订历史 含 §A-rev0 / §A-rev1 / §B-rev0 / §B-rev1 / §C-rev0..§C-rev2 / §D-rev0 / §D-rev1 / §E-rev0 / §E-rev1（含 §E-rev1 §9 procedural follow-through + §5 closure）/ §F-rev0 / §F-rev1。
7. `docs/pluribus_stage2_decisions.md` — D-200..D-283。**Authoritative spec for implementers.**
8. `docs/pluribus_stage2_api.md` — API-200..API-302。**Authoritative spec for stage-2 testers.**

If a change affects a decision or API signature, follow the **D-NNN-revM / API-NNN-revM** amendment flow described in `pluribus_stage1_decisions.md` §10 and `pluribus_stage1_api.md` §11 — append a rev entry, never delete the original, and bump `HandHistory.schema_version` if serialization is affected. Past stage-1 revs（详见各 §10/§11 修订历史）：

- **D-033-rev1** — pin "incomplete raise 不重开 raise option" to TDA Rule 41 / PokerKit-aligned semantics：per-player `raise_option_open: bool`，full raise opens for un-acted players + closes raiser，call/fold closes self only，incomplete touches no flags。Drives `tests/scenarios.rs` #3 (`short_allin_does_not_reopen_raise`, SB-after-BTN-call) vs #4 (`min_raise_chain_after_short_allin`, BTN-after-BB-incomplete)。
- **D-039-rev1** — odd-chip 余 chip 由「逐 1 chip 沿按钮左侧分配」改为「**整笔给按钮左侧最近的获胜者**」（PokerKit 0.4.14 chips-pushing divmod 语义）。每个 pot 独立计算；`payouts()` 行为变化但公开签名不变；`HandHistory.schema_version` 不 bump。
- **D-037-rev1**（D2 [实现] 落地）— `last_aggressor` 作用域从「整手最后一次 voluntary bet/raise」收紧到「**最后一条 betting round 内**最后一次 voluntary bet/raise」（PokerKit `_begin_betting` (state.py:3381) 每条街起手清 `opener_index` 语义）。
- **API-001-rev1** — `HandHistory::replay` / `replay_to` 返回 `Result<_, HistoryError>` instead of `RuleError`；`HistoryError::Rule { index, source: RuleError }` wraps 底层 rule error。
- **API-004-rev1**（B2 [实现] stage-2 触发）— `GameState::config(&self) -> &TableConfig` additive 只读 getter（`stack_bucket` 来源 D-211-rev1 所需）。
- **API-005-rev1**（E2 [实现] stage-2 触发 → E2 关闭后 review procedural follow-through 落地）— `RngSource` trait 新增 `fill_u64s(&mut self, dst: &mut [u64])` default-impl 方法（additive；不修改 `next_u64` 既有签名）。byte-equal 不变量：default impl 字节序列严格等价于循环 `next_u64`（D-051 / D-228 / D-237 全部满足）。E2 `src/core/rng.rs:16-30` 改动 + `ChaCha20Rng::fill_u64s` override 用于 `MonteCarloEquity::equity` hot path 减少 vtable dispatch（D-282 SLO 达成）；E2 commit `d21c5d9` 漏 stage 1 API rev 同步，由 §E-rev1 §9 procedural follow-through commit 追认。

## Workflow (multi-agent, strict role boundaries) — applies to all stages

Each stage is organized as `A → B → C → D → E → F`（13 steps）。Stage-1 workflow lives in `docs/pluribus_stage1_workflow.md`；stage-2 workflow lives in `docs/pluribus_stage2_workflow.md`（mirror structure）。Every step is tagged `[决策] / [测试] / [实现] / [报告]` and **role boundaries are enforced**:

- `[测试]` agent writes tests / harness / benchmarks only. **Never modify product code.** If a test reveals a bug, file an issue for `[实现]` to fix.
- `[实现]` agent writes product code only. **Never modify tests.** If a test fails, fix the product code; only edit the test if it has an obvious bug, and only after review.
- `[决策]` and `[报告]` produce or modify docs in `docs/`.

When the user asks you to do stage work, identify which stage and which step (A0 / A1 / B1 / …) the task belongs to and operate within that role。**当前进度**：stage 1 全 13 步闭合，stage 2 A0 / A1 / B1 / B2 / C1 / C2 / D1 / D2 / E1 / E2 闭合，下一步 F1 [测试]。历史角色越界 carve-out（[测试] ↔ [实现] 边界破例追认 / 0 产品代码改动也算 closure / D-NNN-revM 翻语义同 commit 翻测试 / 错误前移单点不变量）逐条记录在 `pluribus_stage1_workflow.md` §修订历史 与 `pluribus_stage2_workflow.md` §修订历史；遇相似情况时直接查那两份文档。

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
- **`canonical_observation_id` 对 (board, hole) 集合的任意输入顺序不变** (D-218-rev1 / §C-rev2 §4)。`postflop.rs` 在 first-appearance suit remap 之前先按 `Card::to_u8()` 升序排序 board / hole 各自，确保同 (board set, hole set) 任意输入顺序得到同一 canonical id。`tests/canonical_observation.rs::canonical_observation_id_input_shuffle_invariance_*` 是 regression guard。

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

# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository status

Stage 1 of an 8-stage Pluribus-style 6-max NLHE poker AI. **Step C1 is done**: B2 had landed the full product side (state machine / evaluator / history) and 17 driving tests; C1 ([测试] agent) layered the §C1 acceptance harness on top **without touching product code**:

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

C1 出口数据（截至本仓库 commit）：

- `cargo test`（默认）：61 tests passed / 6 ignored / 0 failed across 12 crates；耗时 ~25s。
- `cargo test -- --ignored` 中 `cross_lang_full_10k` 实跑：10,000/10,000 matched, 0 diverged。
- `cargo fmt --all --check`、`cargo clippy --all-targets -- -D warnings`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` 全绿。

Build/test/lint commands are valid as of C1 closure:

- `./scripts/setup-rust.sh` — idempotent rustup install. Pins to `rust-toolchain.toml` (currently `1.95.0`).
- `cargo build --all-targets`
- `cargo fmt --all --check`
- `cargo clippy --all-targets -- -D warnings`
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
- `cargo test --no-run` — compile tests. C1 闭合后 ships 67 tests across 12 crates：`api_signatures` (1) + `cross_eval` (1+1 ignored) + `cross_lang_history` (1+1 ignored) + `cross_validation` (3) + `determinism` (4) + `evaluator` (8+3 ignored) + `fuzz_smoke` (3) + `history_roundtrip` (3+1 ignored) + `scenarios` (10) + `scenarios_extended` (19) + `side_pots` (8)。
- `cargo test` — 默认 61/61 全绿。需要外部依赖的两条交叉验证 (`cross_eval` 类别 vs PokerKit / `cross_validation` 100-hand B2 出口) 在 `pokerkit==0.4.14` + Python ≥3.11 不可用时自动 skipped；C2 出口必须在装好 PokerKit 的环境跑过一次确认 0 分歧。
- `cargo test -- --ignored` — 6 个 full-volume 测试：评估器 1M 一致性 / 反对称 / 传递（运行需性能 evaluator，naive 下耗时较长，留 D2 / E2 回归）；`history_roundtrip_full_100k` / `cross_lang_full_10k` / `cross_eval_full_100k`。当前可在 naive evaluator 下完成的：cross_lang_10k（已实跑通过）。其它 full-volume 在 B2 naive 下耗时长但可跑。
- `cargo bench --bench baseline` — 仍为 B1 占位；E1/E2 替换。

Step **C2** (`[实现]` agent — 让 C1 中遗留的 corner case 全部通过) is next. C1 落地后默认套件已全绿，所以 C2 的"显式驱动"形式会是：装好 PokerKit 跑 `cargo test -- --ignored` + B2 100 手 PokerKit cross-validation 的扩展版本（C1 没扩规模，C2 接到 100k；按 D-085）。Stages 2–8 source code does not exist yet.

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

When the user asks you to do stage-1 work, identify which step (A0 / A1 / B1 / …) the task belongs to and operate within that role. The current step is the highest-numbered step whose outputs already exist in the repo — currently **B2 done** (`GameState` / evaluator / history fully implemented; 10 scenarios + 100-hand PokerKit cross-validation + 10k fuzz all green; D-039-rev1 aligned odd-chip semantics with PokerKit 0.4.14); **C1 has not started**. Note: B2 closure crossed the [实现]→[测试] boundary by completing two test files that B1 had deliberately left as stubs (cross_validation strict comparator + 10k-hand fuzz exit test) — see `docs/pluribus_stage1_workflow.md` §修订历史 B-rev1 for the written acknowledgment and the policy for future analogous situations.

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

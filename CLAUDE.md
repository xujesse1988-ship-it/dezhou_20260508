# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository status

Stage 1 of an 8-stage Pluribus-style 6-max NLHE poker AI. **Step B2 is done**: B1 had stubbed all `[测试]` deliverables on top of A1's `unimplemented!()` API; B2 filled the product side and brought every B1 harness from "skeleton + skipped" to "full pass":

- `src/rules/state.rs` — `GameState` 完整状态机：`legal_actions()`（含 short all-in / min-raise 链 / D-033-rev1 raise-option-open 标记）+ `apply()`（街转换、betting round 推进、摊牌）+ `payouts()`（main pot / side pot / **D-039-rev1** odd-chip 整笔分配 / uncalled bet）。
- `src/eval.rs` — naive `HandEvaluator`：5-card 直接枚举 + 7-choose-5 组合，10k eval/s 量级（按 D-073 故意保留朴素实现，性能优化留给 E2）。
- `src/history.rs` — `HandHistory` 序列化（protobuf via `prost`）+ 反序列化 + `replay_to(action_index)` 任意 index 恢复。
- `tools/pokerkit_replay.py` — PokerKit 0.4.14 完整翻译（dead-button 模式 + 显式 hole/board feed），不再返回 B1Stub。
- 测试侧由 B2 顺手补完两处 B1 留白：`tests/cross_validation.rs` 把 `naive_payouts_match` trip-wire 升级成 strict serde_json 比对 + 新增 100 手 PokerKit 出口测试；`tests/fuzz_smoke.rs` 新增 10k 手 B2 出口测试。该补全跨越 [实现] / [测试] 角色边界，已由 `docs/pluribus_stage1_workflow.md` §修订历史 B-rev1 书面追认。
- 文档：D-039-rev1（decisions §10）把 odd-chip 余 chip 改为「整笔给按钮左侧最近的获胜者」，对齐 PokerKit 0.4.14 默认 chips-pushing divmod 语义；公开 API 签名不变，`HandHistory.schema_version` 不 bump；`pluribus_stage1_validation.md` §3 同步措辞。

Build/test/lint commands are valid as of B2 closure:

- `./scripts/setup-rust.sh` — idempotent rustup install. Pins to the version in `rust-toolchain.toml` (currently `1.95.0`).
- `cargo build --all-targets`
- `cargo fmt --all --check`
- `cargo clippy --all-targets -- -D warnings`
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`
- `cargo test --no-run` — compile tests. B2 ships 17 tests across 4 crates: `api_signatures` (1, spec-drift trip-wire) + `cross_validation` (3：1-smoke / 10-mini-batch / 100-hand PokerKit B2 出口) + `fuzz_smoke` (3：1-smoke / 10-mini-batch / 10k-hand B2 出口) + `scenarios` (10).
- `cargo test` — 17/17 全绿。100 手 PokerKit 出口测试需要 PATH 上有装了 `pokerkit==0.4.14` 的 `python3`（要求 Python ≥3.11）；环境缺失时该测试自动 fallback 到 skipped 而非 fail，但出口验收必须在装好 PokerKit 的环境跑过一次确认 0 分歧。
- `cargo bench --bench baseline` — placeholder 仍是 B1 留下的 `catch_unwind` 包装，跑出占位 ns 数据；真实 hot-path bench + SLO 断言由 E1 / E2 接管。

Step **C1** (`[测试]` agent — 把 fixed scenario 扩到 200+ / side pot 扩到 100+ / 评估器与开源参考交叉验证 1M 手 / hand history 100k 手 roundtrip / 跨语言反序列化 / 确定性测试) is next. Stages 2–8 source code does not exist yet.

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

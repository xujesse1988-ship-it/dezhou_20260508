# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository status

This repo is currently **documentation-only**. No source code, no `Cargo.toml`, no build/test commands exist yet. The current task is Stage 1 of an 8-stage Pluribus-style 6-max NLHE poker AI implementation. Source code (Rust) will be added starting from step A1 in `docs/pluribus_stage1_workflow.md`.

Do not invent commands (`cargo build`, `make`, etc.) until the corresponding scaffolding has been committed.

## Documents and their authority

The four stage-1 docs form a contract hierarchy. Read them in this order before making changes:

1. `docs/pluribus_path.md` — overall 8-stage roadmap, stage acceptance gates, hardware/time budgets. Stages 4–6 thresholds are deliberately stricter than the original Pluribus path; do **not** weaken them.
2. `docs/pluribus_stage1_validation.md` — quantitative pass criteria for stage 1 (e.g. 1M-hand fuzz, ≥10M eval/s, 100k-hand cross-validation with PokerKit).
3. `docs/pluribus_stage1_decisions.md` — locked technical/rule decisions (D-001 … D-103). **Authoritative spec for implementers.**
4. `docs/pluribus_stage1_api.md` — locked Rust API contract (API-NNN). **Authoritative spec for testers.**

If a change affects a decision or API signature, you must follow the **D-100 / API-NNN-revM** amendment flow described in `pluribus_stage1_decisions.md` §10 — append a `D-NNN-revM` entry, never delete the original, and bump `HandHistory.schema_version` if serialization is affected.

## Stage-1 workflow (multi-agent, strict role boundaries)

Stage 1 work is organized as `A → B → C → D → E → F` (13 steps, see `docs/pluribus_stage1_workflow.md`). Every step is tagged `[决策] / [测试] / [实现] / [报告]` and **role boundaries are enforced**:

- `[测试]` agent writes tests / harness / benchmarks only. **Never modify product code.** If a test reveals a bug, file an issue for `[实现]` to fix.
- `[实现]` agent writes product code only. **Never modify tests.** If a test fails, fix the product code; only edit the test if it has an obvious bug, and only after review.
- `[决策]` and `[报告]` produce or modify docs in `docs/`.

When the user asks you to do stage-1 work, identify which step (A0 / A1 / B1 / …) the task belongs to and operate within that role. The current step is the highest-numbered step whose outputs already exist in the repo — currently A0 (decisions + API doc are committed; A1 scaffolding has not started).

## Non-negotiable invariants (apply to all stage-1 code)

These are repeated across the decision and validation docs because violations corrupt downstream CFR training and are nearly impossible to debug post-hoc:

- **No floating point in rules, evaluator, history, or abstraction.** Chips are integer `u64` (`ChipAmount`); P&L is `i64`. A PR that introduces `f32`/`f64` in these paths must be rejected (D-026).
- **No global RNG.** All randomness goes through an explicit `RngSource` parameter (D-027, D-050).
- **`ChipAmount::Sub` panics on underflow** in both debug and release (D-026b). Callers needing saturating semantics must use `checked_sub` explicitly.
- **`Action::Raise { to }` is an absolute amount**, not a delta — matches NLHE protocol convention.
- **Showdown `last_aggressor`** counts only voluntary bets/raises (blinds, antes, preflop limps don't count) (D-037).
- **Short all-in does not reopen raise option.** This is the most error-prone NLHE rule and has dedicated test coverage (D-033, validation §1, scenario `short_allin_does_not_reopen_raise`).
- **Determinism baseline:** same architecture + toolchain + seed → identical hand-history BLAKE3 hash. Cross-architecture (x86 vs ARM) is an aspirational goal, not a stage-1 pass criterion (D-051, D-052).

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

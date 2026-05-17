# Repository Guidelines

## Project Direction

This repository is a Rust 2021 crate named `poker`. The current product target is a heads-up No-Limit Texas Hold'em solver, documented in `docs/heads_up_nlhe_solver_target.md`. The older Pluribus-style 6-max path in `docs/pluribus_path.md` is long-term reference material, not the default near-term acceptance target.

Keep core APIs extensible toward 6 players: do not replace `TableConfig.n_seats`, `SeatId`, dynamic player vectors, or payoff vectors with heads-up-only concepts. Training quality gates and formal evaluations should focus on 2-player NLHE unless the target document changes.

## Project Structure & Module Organization

Core library code lives in `src/`: `core/` for primitive types and RNG, `rules/` for table config/actions/state, `abstraction/` for action and information abstraction plus bucket tables, `training/` for game traits, CFR/MCCFR, checkpointing, and game adapters, plus `eval.rs`, `history.rs`, and `error.rs`. Integration tests live in `tests/`, with shared fixtures in `tests/common/`. Benchmarks are in `benches/`, protobuf schema in `proto/`, helper scripts in `scripts/`, Python cross-validation tooling in `tools/`, and design/API contracts in `docs/`.

## Build, Test, and Development Commands

- `./scripts/setup-rust.sh`: install the pinned Rust toolchain and required components.
- `cargo build --all-targets`: compile library, tests, and benches.
- `cargo fmt --all --check`: verify formatting.
- `cargo clippy --all-targets -- -D warnings`: run lint checks at CI strictness.
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`: build docs and reject rustdoc warnings.
- `cargo test --no-run`: compile all tests without running them.
- `cargo test`: run the default test suite.
- `cargo bench --bench baseline`: run Criterion benchmark placeholders.

## Coding Style & Naming Conventions

Use Rust 2021 with the pinned toolchain in `rust-toolchain.toml` (`1.95.0`). Formatting is controlled by `rustfmt.toml`; do not hand-format around rustfmt. `unsafe` is forbidden by `Cargo.toml`. Keep Rust identifiers and inline comments in English. Repository docs and commit messages are Chinese, matching existing history.

## Testing Guidelines

Add integration tests under `tests/` and shared helpers under `tests/common/`. Keep public API signature changes synchronized with `tests/api_signatures.rs`. Heads-up rule/profile changes should be covered by focused tests like `tests/heads_up_rules.rs`. Scenario tests should encode rule contracts from the relevant decision/API docs. CI requires at least `cargo test --no-run`; run narrower tests for the files you touch and broader tests when changing shared behavior.

## Commit & Pull Request Guidelines

Recent commits use concise Chinese messages with optional conventional prefixes, for example `feat: ...`, `chore: ...`, and `docs(CLAUDE.md): ...`. PRs should state the stage/workflow step, summarize behavior changes, list commands run, and link any amended decision/API entries. If UI or docs are affected, include screenshots or rendered snippets where useful.

## Agent-Specific Instructions

Respect role boundaries from `CLAUDE.md` when a task explicitly assigns a stage role: test agents edit tests/harness/benchmarks only, implementation agents edit product code, and decision/report agents edit docs. For decision or API changes, append revision entries instead of deleting historical decisions.

After modifying repository files, agents should verify the change, then commit and push by default unless the user explicitly says not to commit or not to push.

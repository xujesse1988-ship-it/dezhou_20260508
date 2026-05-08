# Repository Guidelines

## Project Structure & Module Organization

This repository is a Rust 2021 crate named `poker` for stage 1 of a Pluribus-style 6-max NLHE poker AI. Core library code lives in `src/`: `core/` for primitive types and RNG, `rules/` for table config/actions/state, plus `eval.rs`, `history.rs`, and `error.rs`. Integration tests live in `tests/`, with shared fixtures in `tests/common/`. Benchmarks are in `benches/`, protobuf schema in `proto/`, helper scripts in `scripts/`, Python cross-validation tooling in `tools/`, and design/API contracts in `docs/`.

## Build, Test, and Development Commands

- `./scripts/setup-rust.sh`: install the pinned Rust toolchain and required components.
- `cargo build --all-targets`: compile library, tests, and benches.
- `cargo fmt --all --check`: verify formatting.
- `cargo clippy --all-targets -- -D warnings`: run lint checks at CI strictness.
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`: build docs and reject rustdoc warnings.
- `cargo test --no-run`: compile all tests without running stage-1 stubs that still panic.
- `cargo test`: run tests; expect scenario tests to fail until B2 implements the stubs.
- `cargo bench --bench baseline`: run Criterion benchmark placeholders.

## Coding Style & Naming Conventions

Use Rust 2021 with the pinned toolchain in `rust-toolchain.toml` (`1.95.0`). Formatting is controlled by `rustfmt.toml`; do not hand-format around rustfmt. `unsafe` is forbidden by `Cargo.toml`. Keep Rust identifiers and inline comments in English. Repository docs and commit messages are Chinese, matching existing history.

## Testing Guidelines

Add integration tests under `tests/` and shared helpers under `tests/common/`. Keep public API signature changes synchronized with `tests/api_signatures.rs`. Scenario tests should encode rule contracts from `docs/pluribus_stage1_decisions.md` and `docs/pluribus_stage1_api.md`. CI currently requires compilation via `cargo test --no-run`; runnable test coverage expands as implementation replaces `unimplemented!()` stubs.

## Commit & Pull Request Guidelines

Recent commits use concise Chinese messages with optional conventional prefixes, for example `feat: ...`, `chore: ...`, and `docs(CLAUDE.md): ...`. PRs should state the stage/workflow step, summarize behavior changes, list commands run, and link any amended decision/API entries. If UI or docs are affected, include screenshots or rendered snippets where useful.

## Agent-Specific Instructions

Respect stage role boundaries from `CLAUDE.md`: test agents edit tests/harness/benchmarks only, implementation agents edit product code, and decision/report agents edit docs. For decision or API changes, append revision entries instead of deleting historical decisions.

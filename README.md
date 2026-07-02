# poker — No-Limit Texas Hold'em Solver (Rust)

> 中文版见 [README.zh-CN.md](./README.zh-CN.md)

A reproducible, trainable, and evaluatable **No-Limit Texas Hold'em** solver written in Rust.
The **heads-up (2-player) 200BB** solver is the completed baseline; the **6-max (6-player) 100BB
blueprint** track is the current main line. Core APIs (rules engine, seat model, abstraction,
training traits, payoff vectors) are kept `n_seats`-generic so nothing has to be rewritten when
moving between 2 and 6 players.

- **Language / stack**: Rust 2021, pinned toolchain `1.95.0` (`rust-toolchain.toml`), `unsafe` forbidden.
- **Algorithm**: External-Sampling MCCFR / LCFR (Brown & Sandholm 2018 Discounted MCCFR), dense
  tabular backend, streaming checkpoints, information abstraction (169 lossless preflop + equity/OCHS
  postflop buckets).
- **Evaluation**: LBR / best-response (heads-up), AIVAT variance-reduced head-to-head, plus live play
  against Slumbot (HUNL) and OpenPoker (6-max).

---

## Current Status

### Heads-up NLHE (stages H1–H5) — wrapped up ✅

A 1B-update dense blueprint (200BB) plays **near break-even against Slumbot**: over 10,000 AIVAT hands,
raw −85.25 / AIVAT −108.31 mbb/g with confidence intervals crossing 0 (as expected, not statistically
significant). The full LCFR / batched-parallel / dense backend + v4 bucket + AIVAT evaluation chain is
verified end-to-end. Only trailing action left is ongoing Slumbot battle-data collection.
See [`docs/heads_up_nlhe_solver_target.md`](./docs/heads_up_nlhe_solver_target.md).

### 6-max NLHE blueprint-only (route A, 100BB) — main line 🚧

Kicked off 2026-05-30. Route A = get the offline self-play blueprint working end-to-end first
(parameterized game → multi-way abstraction → reuse the N-generic trainer → real head-to-head
evaluation), **without** real-time depth-limited search as a hard requirement.

| Stage | Focus | Status |
|---|---|---|
| S1 | Rules / 6-max profile | closed (100k PokerKit re-run pending) |
| S2 | Tree sizing + A3×A4 abstraction into production | closed |
| S3 | Multi-way bucketing (single-opponent buckets reusable ≤3-way) | closed |
| S4 | 1B dense training + preflop reshape (`--reshape none\|nolimp\|preopen\|preopen-small`) | one round done + independently reviewed |
| S5 | Off-tree cross-abstraction advisor engine, cross-abstraction h2h, live OpenPoker client | end-to-end smoke passed |
| S6 | Real-time subgame search MVP | core landed + verified on a branch (not merged) |

Key findings on the 6-max line:

- **6-max is a multi-player general-sum game**, so CFR self-play no longer provably converges to Nash,
  and LBR/exploitability lose their theoretical meaning (kept only as diagnostics). Quality is judged by
  **real head-to-head play**, not by an exploitability floor.
- **Preflop reshape** (dropping non-SB limps + adding a 2.25BB open size) cleaned up preflop dominated-pair
  flips from ~13% → <1% and shrank the tree up to ~4.2×. A GTO Wizard truth-check confirmed SB limp / AA-limp
  are correct GTO, not defects.
- **Real-time search MVP**: the bottleneck is **blueprint / abstraction quality**, not the search root —
  clean, well-trained nodes (flop-first) stay neutral, while naively widening the trigger to all post-flop
  nodes regresses on a weak base.
- **Exploitation Tier 2** (in-process opponent profiling → convergence gate → preflop-width range tilt on the
  unanchored search path) is behind `--exploit on|vpip|off`, defaulting to `off` (byte-equal with the shipped
  strategy when off).

See [`docs/six_max_nlhe_target.md`](./docs/six_max_nlhe_target.md) (main-line acceptance target) and
[`docs/status_v3.md`](./docs/status_v3.md) (ground-truth code status).

---

## Verified Algorithm Correctness

This is the reusable foundation the 6-max line builds on. Each row has an external cross-check
(invariant #7: no algorithm change ships without one).

| Item | Status | Evidence |
|---|---|---|
| Kuhn / Leduc Vanilla CFR | ✅ converges to closed-form `-1/18`, exploitability `<0.1` | `tests/cfr_kuhn.rs`, `tests/cfr_leduc.rs` |
| Leduc ES-MCCFR / LCFR-MCCFR | ✅ `ev_p0` → -0.087; ES path BLAKE3 byte-equal anchor | `tools/leduc_es_mccfr_report` |
| Simplified NLHE ES-MCCFR / LCFR | ✅ LCFR 100M LBR 1,233 → 500M 1,126 (saturates by 100M) | `run_lcfr_*` (vultr) |
| Dense backend + v4 bucket | ✅ byte-equal vs HashMap; ~2.2× throughput, flat 5.2 GiB RAM, no checkpoint blowup | `tests/dense_nlhe_trainer.rs` |
| AIVAT evaluator | ✅ unbiased (full proof); 1.21× variance reduction on real logs | `tests/aivat_nlhe_*.rs`, `docs/aivat_eval.md` |
| CFR trainer / rules engine, 6-max N-generic | ✅ multi-way side pot returns per-seat payoff vector; traverser rotates `% n_players` | `src/training/trainer.rs`, `src/rules/state.rs` |

---

## Repository Layout

```
src/
  core/         primitive types (Card, ChipAmount, SeatId, Street, ...) + explicit RngSource
  rules/        table config, actions, state machine, side pots, showdown
  abstraction/  action abstraction + info abstraction, preflop 169, equity/OCHS, buckets, InfoSetId
  training/     Game trait, CFR/MCCFR trainer, checkpoints, NLHE game adapters,
                blueprint advisor, subgame search, opponent profiling, AIVAT/LBR eval
  eval.rs history.rs error.rs lib.rs
tests/          integration tests + cross-validation (run on remote host)
tools/          diagnostic / training / live-play binaries (see below) + Python helpers
benches/        Criterion benchmarks
proto/          protobuf schema (hand history)
scripts/        setup + deploy + cross-validation helpers
docs/           design docs, acceptance targets, decision/API records (Chinese)
```

### Notable binaries (`tools/`, declared in `Cargo.toml`)

- **Training**: `train_cfr`
- **6-max eval / experiments**: `six_max_eval`, `six_max_blueprint_h2h`, `six_max_search_probe`,
  `six_max_exploit_ab`, `six_max_cross_street_ab`, `six_max_unanchored_prefix_ab`
- **Live play (advisors + drivers)**: `slumbot_advisor` + `tools/slumbot_play.py` (HUNL),
  `openpoker_advisor` + `tools/openpoker_play.py` (6-max WebSocket)
- **AIVAT**: `aivat_build_values`, `aivat_eval`, `openpoker_hh_aivat`
- **Buckets / abstraction**: `bucket_kmeans_fit`, `bucket_quality_dump`, `bucket_table_reindex_v3_to_v4`,
  `bucket_features_dump`
- **Reproducibility / diagnostics**: `b3sum`, `nlhe_blake3_anchor`, `nlhe_checkpoint_vs_checkpoint`,
  `nlhe_betting_tree_sizing`, `leduc_es_mccfr_report`, `mccfr_trace`, `nlhe_trace`

---

## Quick Start

```bash
# One-time: install the pinned Rust toolchain (rustup + 1.95.0 + rustfmt + clippy). Idempotent.
./scripts/setup-rust.sh
# Load cargo into the current shell after a fresh install (new shells pick it up automatically).
. "$HOME/.cargo/env"

# Build / lint / format gates
cargo build --all-targets
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings

# Tests
cargo test                       # default suite
cargo test --release -- --ignored  # long-running perf/correctness SLOs + BLAKE3 anchors
```

Optional PokerKit cross-validation (used by 6-max stage S1):

```bash
uv venv --python 3.11 .venv-pokerkit
uv pip install --python .venv-pokerkit/bin/python "pokerkit==0.4.14"
PATH=".venv-pokerkit/bin:$PATH" cargo test
```

> **Where tests run**: the local machine is only trusted for `build` / `fmt` / `clippy`. Full training and
> the long test suite run on a remote host (results from an underpowered local box are not trustworthy).

---

## Hard Invariants (enforced by the compiler / clippy / `Cargo.toml`)

See [`docs/invariants.md`](./docs/invariants.md). A PR violating these does not pass.

1. **No floats** in the rules / evaluator / abstraction layers — chips are `u64`, payoffs `i64`, hand rank is
   an integer, bucket ids are discrete. Floats are only allowed inside CFR σ/regret accumulation.
2. **No global RNG** — all randomness flows through an explicit `RngSource`; byte-equal reproducibility is the
   minimum bar for catching algorithm bugs.
3. **No `unsafe`** — `unsafe_code = "forbid"` in `Cargo.toml`.
4. **`ChipAmount::Sub` underflow panics** (debug + release) — a negative chip count is always a bug; use
   `checked_sub` for saturating behavior.
5. **`Action::Raise { to }` is absolute** — `to` is the target amount (including chips already in), matching
   the NLHE / PokerKit convention.
6. **One seat-direction convention** — `SeatId((k+1) mod n_seats)` is the left neighbor of `SeatId(k)`;
   every "to the left" rule (button rotation, blinds, odd-chip, showdown order, deal start) uses it.

---

## Infrastructure

| Host | Role | Notes |
|---|---|---|
| vultr (4 vCPU / 11.67 GiB) | persistent storage + short tests | holds 1B dense checkpoints + bucket tables; **cannot run NLHE training** (3M updates hit swap) |
| AWS (on-demand, IP varies) | training | HU used `c6a.8xlarge` (32 vCPU); 6-max likely needs a bigger box, sized in S2 |

Persistent artifacts (1B dense checkpoint, bucket tables) live under `~/dezhou_20260508/artifacts/` on vultr.

---

## Documentation Map

Read in order of authority (highest first):

1. [`docs/status_v3.md`](./docs/status_v3.md) — ground-truth code status. **Read the correctness table before touching code.**
2. [`docs/invariants.md`](./docs/invariants.md) — hard code-level constraints.
3. [`docs/six_max_nlhe_target.md`](./docs/six_max_nlhe_target.md) — current main-line target (6-max blueprint-only, S1–S6 gates).
4. [`docs/heads_up_nlhe_solver_target.md`](./docs/heads_up_nlhe_solver_target.md) — heads-up phase (H1–H5), wrapped up.
5. [`docs/aivat_eval.md`](./docs/aivat_eval.md) — AIVAT evaluator details.
6. [`CLAUDE.md`](./CLAUDE.md) / [`AGENTS.md`](./AGENTS.md) — repo navigation and working rules for coding agents.

---

## Working Language

Docs and commit messages are in Chinese; Rust identifiers and inline comments are in English (Rust convention).

## License

MIT OR Apache-2.0.

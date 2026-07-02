# poker — a No-Limit Hold'em solver in Rust

> 中文版见 [README.zh-CN.md](./README.zh-CN.md)

This is a solver for No-Limit Texas Hold'em. It learns a strategy offline by playing hundreds of
millions of hands against itself, then uses that strategy — optionally sharpened by real-time search
— to play actual hands against public bots like Slumbot and OpenPoker.

It started as a heads-up (2-player, 200BB) solver and now shares one codebase with a 6-max (6-player,
100BB) version. The rules engine, seat model, abstraction, and trainer all work for any number of seats,
so the 6-max version runs on the same core and just adds new abstractions and a much larger game tree.

The value that drove most decisions is correctness: everything is checked against outside ground truth
and reproducible byte-for-byte. Built with Rust 2021, toolchain pinned to `1.95.0`, `unsafe` forbidden.

---

## Results at a glance

| Track | Setup | Result |
|---|---|---|
| Heads-up 200BB | 1B-update blueprint vs Slumbot, 10,000 AIVAT hands | raw −85.25 / AIVAT −108.31 mbb/g — CI crosses 0, near break-even |
| 6-max 100BB | blueprint vs the live OpenPoker pool | evaluation ongoing; no strong public reference bot to measure against |

Heads-up is the finished baseline; 6-max is the current line of work. Full numbers and method are in
[`docs/heads_up_nlhe_solver_target.md`](./docs/heads_up_nlhe_solver_target.md) and
[`docs/six_max_nlhe_target.md`](./docs/six_max_nlhe_target.md).

---

## The hard problems

Building a No-Limit Hold'em solver mostly comes down to getting past a handful of problems that don't
have clean textbook answers. Here is each one, and the choice we made.

**1. The game is far too big to solve directly.** Full heads-up Hold'em has more distinct situations
than any table could hold, so the first job is compressing it into something trainable while keeping the
distinctions that matter. Preflop compresses for free: the 1,326 starting hands fall into 169 classes
(pairs, suited, offsuit), and since suits are interchangeable before the flop, that grouping is exact.
Postflop is lossy — each hand becomes a short feature vector (an equity histogram plus opponent-cluster
hand strength) that k-means sorts into buckets, 1,000 per street heads-up and 200 for 6-max, numbered
weakest to strongest. The subtlety: similar-looking hands play very differently on textured boards, so
opponent-cluster strength is computed at the individual-combo level, which keeps monotone and paired
boards honest.

**2. Bet-size abstraction is a knife-edge.** Too few sizes and the strategy is easy to exploit; too many
and the tree explodes past what you can train. We measured it rather than guessed — adding a single
half-pot bet size multiplied the 6-max tree by more than 20×. So sizes are pot-relative and deliberately
sparse, postflop action is capped at three-way, and a preflop reshape drops the limps that added
branches without adding much strategy and swaps in one clean open size. That reshape cut preflop
dominated-pair flips from ~13% to under 1% and shrank the tree by up to 4.2×. (A GTO Wizard check
confirmed the small-blind limps we kept — including limping AA — are correct GTO, not artifacts.)

**3. You can't tell by looking whether a sampling solver is correct.** The blueprint is trained by
External-Sampling MCCFR, optionally with Linear CFR discounting (Brown & Sandholm, 2018), on a dense
backend to roughly a billion updates. MCCFR is Monte Carlo, so a subtle bug doesn't crash — it just
converges to a slightly wrong strategy that looks fine from the outside. The defense is to make every
run reproducible byte-for-byte from its seed and to pin every algorithm to an outside ground truth: the
tiny games Kuhn and Leduc have closed-form answers (the trainer lands exactly on −1/18), PokerKit
cross-checks the rules engine, and byte-equal anchors catch any drift between backends.

**4. Six players break the theory heads-up relies on.** Two-player poker is zero-sum, so CFR provably
approaches a Nash equilibrium and "exploitability" is a real number you can drive toward zero. Six-max
is multiplayer general-sum, where none of that holds — self-play has no equilibrium guarantee and
exploitability stops being meaningful. So strength is judged by real play against outside bots rather
than by self-play scores. What still transfers is the abstraction: we checked empirically that the
heads-up single-opponent buckets stay valid up to three-way (river hand-ranking correlates at Spearman
0.9995), so 6-max builds on the same buckets instead of a separate scheme.

**5. Variance hides whether you're actually winning.** Poker is noisy enough that a few thousand hands
can't separate a real edge from luck. Heads-up uses AIVAT, an unbiased variance-reduction method that
squeezes a roughly 1.2× tighter estimate out of the same real-game logs. Six-max has no strong public
reference bot to measure against, so evaluation there is live play against the OpenPoker pool.

**6. Real-time search is where the strength is, and where it's easiest to make things worse.** The
strongest modern bots search the current spot at the table instead of only looking it up. But the safety
guarantees behind that (DeepStack-style re-solving) don't survive in multiplayer general-sum, and search
on a weak base actively loses. Our subgame search is a compact, depth-limited solve in the spirit of
Pluribus and Modicum, reusing the same trainer on just the subtree. The honest finding: the bottleneck
is blueprint and abstraction quality, not the search itself — a conservative trigger (the first flop
decision) stays neutral, while widening it regressed. So search ships conservative, and the real lever
is a better blueprint. A separate opt-in mode profiles opponents as it plays and gently tilts ranges
against ones it has seen enough of; it's off by default and byte-identical to the plain strategy when off.

---

## Build and run

```bash
# One-time: install the pinned Rust toolchain (rustup + 1.95.0 + rustfmt + clippy). Idempotent.
./scripts/setup-rust.sh
. "$HOME/.cargo/env"   # load cargo into the current shell after a fresh install

# Build / lint / format gates
cargo build --all-targets
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings

# Tests
cargo test                          # default suite
cargo test --release -- --ignored   # long-running perf/correctness SLOs + BLAKE3 anchors
```

The full pipeline is: dump hand features → fit bucket tables → train the blueprint → evaluate → play a
live opponent. Every tool parses plain space-separated flags (no `--flag=value`) and takes `--help`.

```bash
# 1. Dump per-street hand features
cargo run --release --bin bucket_features_dump -- --street flop  --output artifacts/features_flop.bin
cargo run --release --bin bucket_features_dump -- --street turn  --output artifacts/features_turn.bin
cargo run --release --bin bucket_features_dump -- --street river --output artifacts/features_river.bin

# 2. Fit the bucket table with k-means (heads-up 1000 per street; 6-max uses 200)
cargo run --release --bin bucket_kmeans_fit -- \
  --feature-flop artifacts/features_flop.bin --feature-turn artifacts/features_turn.bin \
  --feature-river artifacts/features_river.bin \
  --bucket-flop 1000 --bucket-turn 1000 --bucket-river 1000 \
  --training-seed 0xcafebabe --output artifacts/bucket_table.bin

# 3. Train the blueprint (heavy — this runs on a remote host; see scripts/deploy-aws-training.sh)
#    6-max 100BB, A3×A4 with the preopen reshape:
cargo run --release --bin train_cfr -- --game nlhe --trainer es-mccfr --dense --lockfree \
  --profile six-max --postflop-cap 3 --reshape preopen \
  --bucket-table artifacts/bucket_table_200_200_200_seed_cafebabe_schemav4.bin \
  --updates 10000000000 --lcfr-period 1000000000 --checkpoint-dir artifacts/run_6max

# 4. Evaluate: 6-max baseline gate, or heads-up AIVAT
cargo run --release --bin six_max_eval -- \
  --bucket-table artifacts/bucket_table_200_200_200_seed_cafebabe_schemav4.bin \
  --checkpoint artifacts/run_6max/nlhe_es_mccfr_final_010000000000.ckpt \
  --postflop-cap 3 --hands-per-seat 170000

cargo run --release --bin aivat_build_values -- --checkpoint <ckpt> --bucket-table <bt> --out artifacts/aivat_values.bin
cargo run --release --bin aivat_eval -- --checkpoint <ckpt> --bucket-table <bt> \
  --vf artifacts/aivat_values.bin --strategy-log slumbot_strategy.jsonl

# 5. Play a live opponent (the Python driver spawns the Rust advisor over stdio JSON)
python3 tools/slumbot_play.py   --checkpoint <ckpt> --bucket-table <bt> --username <u> --password <p> --num-hands 1000
python3 tools/openpoker_play.py --checkpoint <ckpt> --bucket-table <bt> --reshape preopen --postflop-cap 3 --api-key <key> --num-hands 1000
```

The 6-max advisor also exposes real-time search and opponent-exploitation flags (`--search*`,
`--exploit on|vpip|off`, default off) — see [`docs/six_max_nlhe_target.md`](./docs/six_max_nlhe_target.md)
for the full set. Both drivers accept `--selftest` for an offline IPC check with no account.

Optional PokerKit cross-validation:

```bash
uv venv --python 3.11 .venv-pokerkit
uv pip install --python .venv-pokerkit/bin/python "pokerkit==0.4.14"
PATH=".venv-pokerkit/bin:$PATH" cargo test
```

> The local machine is only trusted for `build` / `fmt` / `clippy`. Real training and the long test
> suite run on a remote host — results from an underpowered local box aren't trustworthy.

---

## Repository layout

```
src/
  core/         primitive types (Card, ChipAmount, SeatId, Street, ...) + explicit RngSource
  rules/        table config, actions, state machine, side pots, showdown
  abstraction/  action abstraction + info abstraction, preflop 169, equity/OCHS, buckets, InfoSetId
  training/     Game trait, CFR/MCCFR trainer, checkpoints, NLHE game adapters,
                blueprint advisor, subgame search, opponent profiling, AIVAT/LBR eval
  eval.rs history.rs error.rs lib.rs
tests/          integration tests + cross-validation (run on remote host)
tools/          diagnostic / training / live-play binaries + Python drivers
benches/        Criterion benchmarks
proto/          protobuf schema (hand history)
scripts/        setup + deploy + cross-validation helpers
docs/           design docs, acceptance targets, decision/API records (Chinese)
```

Notable binaries (in `tools/`, declared in `Cargo.toml`):

- Training: `train_cfr`
- Buckets / abstraction: `bucket_features_dump`, `bucket_kmeans_fit`, `bucket_quality_dump`,
  `bucket_table_reindex_v3_to_v4`
- 6-max eval / experiments: `six_max_eval`, `six_max_blueprint_h2h`, `six_max_search_probe`,
  `six_max_exploit_ab`, `six_max_cross_street_ab`, `six_max_unanchored_prefix_ab`
- AIVAT: `aivat_build_values`, `aivat_eval`, `openpoker_hh_aivat`
- Live play: `slumbot_advisor` + `tools/slumbot_play.py` (HUNL),
  `openpoker_advisor` + `tools/openpoker_play.py` (6-max WebSocket)
- Reproducibility / diagnostics: `b3sum`, `nlhe_blake3_anchor`, `nlhe_checkpoint_vs_checkpoint`,
  `nlhe_betting_tree_sizing`, `leduc_es_mccfr_report`, `mccfr_trace`, `nlhe_trace`

---

## Testing, validation, and invariants

Every algorithm change ships with an external cross-check — a closed-form answer, a PokerKit comparison,
or a byte-equal anchor from a known-good run. The full evidence grid (which test proves what) is in
[`docs/status_v3.md`](./docs/status_v3.md); the correctness rules a PR must satisfy are in
[`docs/invariants.md`](./docs/invariants.md). The hard invariants, enforced by the compiler, clippy, and
`Cargo.toml`:

1. No floats in the rules, evaluator, or abstraction layers — chips are `u64`, payoffs `i64`, hand rank
   is an integer, bucket ids are discrete. Floats are only allowed inside CFR σ/regret accumulation.
2. No global RNG — all randomness flows through an explicit `RngSource`; byte-equal reproducibility is the
   minimum bar for catching algorithm bugs.
3. No `unsafe` — `unsafe_code = "forbid"` in `Cargo.toml`.
4. `ChipAmount::Sub` underflow panics (debug and release) — a negative chip count is always a bug; use
   `checked_sub` for saturating behavior.
5. `Action::Raise { to }` is absolute — `to` is the target amount, chips already in included.
6. One seat-direction convention — `SeatId((k+1) mod n_seats)` is the left neighbor of `SeatId(k)`, and
   every "to the left" rule (button rotation, blinds, odd-chip, showdown order, deal start) uses it.

---

## Project status

Heads-up (200BB, stages H1–H5) is the finished baseline: the full training and evaluation chain is
verified end to end, and the blueprint plays near break-even against Slumbot (see results above). Current
activity there is ongoing Slumbot battle-data collection.

6-max blueprint-only (route A, 100BB) is the current line, kicked off 2026-05-30 — get the offline
self-play blueprint working end to end first (parameterized game → multi-way abstraction → seat-generic
trainer → real head-to-head evaluation), with real-time search as a follow-on rather than a hard gate.

| Stage | Focus | Status |
|---|---|---|
| S1 | Rules / 6-max profile | closed (100k PokerKit re-run pending) |
| S2 | Tree sizing + A3×A4 abstraction into production | closed |
| S3 | Multi-way bucketing (single-opponent buckets reusable ≤3-way) | closed |
| S4 | 1B dense training + preflop reshape | one round done + independently reviewed |
| S5 | Cross-abstraction advisor engine, cross-abstraction h2h, live OpenPoker client | end-to-end smoke passed |
| S6 | Real-time subgame search MVP | core landed + verified on a branch (not merged) |

Ground-truth code status: [`docs/status_v3.md`](./docs/status_v3.md).

---

## References

The design follows a well-trodden line of imperfect-information game research.

- Zinkevich, Johanson, Bowling, Piccione (2007). *Regret Minimization in Games with Incomplete Information.* — CFR.
- Lanctot, Waugh, Zinkevich, Bowling (2009). *Monte Carlo Sampling for Regret Minimization in Extensive Games.* — MCCFR.
- Johanson, Burch, Valenzano, Bowling (2013). *Evaluating State-Space Abstractions in Extensive-Form Games.* — OCHS.
- Moravčík et al. (2017). *DeepStack: Expert-Level AI in Heads-Up No-Limit Poker.* Science.
- Brown, Sandholm, Amos (2018). *Depth-Limited Solving for Imperfect-Information Games.* NeurIPS. — Modicum.
- Burch, Schmid, Moravčík, Morrill, Bowling (2018). *AIVAT: A New Variance Reduction Technique for Agent Evaluation.* AAAI.
- Brown, Sandholm (2019). *Solving Imperfect-Information Games via Discounted Regret Minimization.* AAAI. — Discounted/Linear CFR.
- Brown, Sandholm (2019). *Superhuman AI for Multiplayer Poker.* Science. — Pluribus.
- Kim et al. (2023). *PokerKit: A Comprehensive Python Library for Fine-Grained Multi-Variant Poker Game Simulations.* IEEE ToG. — cross-validation reference.

---

## Working language

Docs and commit messages are in Chinese; Rust identifiers and inline comments are in English (Rust
convention).

## License

MIT OR Apache-2.0.

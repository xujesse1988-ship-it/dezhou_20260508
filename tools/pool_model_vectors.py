#!/usr/bin/env python3
"""Export cross-check vectors for the Rust side of the pool model (`src/training/pool_model.rs`).

Takes every N-th stored hand, and for each preflop decision writes what
`opponent_analysis.pool_model.hand_obs` made of it (situation, features, action kind, pot fraction)
next to the bare betting sequence (seat index from the button, fold or new street total). It also
fits a model on those hands and writes it with its answer for every cell seen. The Rust test
`tests/pool_model_vectors.rs` replays the sequences and must reproduce all of it.

No names, cards or hand ids are written.

Usage (repo root):
    python3 tools/pool_model_vectors.py --db artifacts/opponent_db_20261031 --platform wopo \
        --out tests/data [--every 15]
"""

from __future__ import annotations

import argparse
import dataclasses
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from opponent_analysis import db, pool_model  # noqa: E402
from opponent_analysis.replay import ring_from  # noqa: E402


def hand_vector(h):
    """Vector of one hand, or None when its starting street contributions cannot be rebuilt."""
    if len(h.dealt_seats) < 3:
        return None
    ring = ring_from(h.dealt_seats, h.button_seat)
    idx = {s: i for i, s in enumerate(ring)}
    pre = [d for d in h.decisions if d.street == "preflop"]
    if not pre:
        return None
    # Street contribution of each seat before its first action; blinds for seats that never act.
    contrib = {}
    for s in ring:
        p = h.positions[s]
        contrib[s] = h.sb if p == "SB" else (h.bb if p == "BB" else 0)
    seen = set()
    for d in pre:
        if d.seat not in seen:
            seen.add(d.seat)
            contrib[d.seat] = d.to_total - d.delta
    if max(contrib.values()) != pre[0].current_bet:
        return None
    # Every seat as a pool player: no hero seat, a name for everyone.
    anon = dataclasses.replace(h, hero_seat=-1, names={s: f"s{s}" for s in h.dealt_seats})
    obs = pool_model.hand_obs(anon)
    if len(obs) != len(h.decisions):
        return None
    acts = []
    for d, o in zip(h.decisions, obs):
        if d.street != "preflop":
            break
        acts.append({
            "seat": idx[d.seat], "fold": d.kind == "fold", "to": d.to_total,
            "family": o.family, "dims": o.dims, "kind": d.kind, "frac": o.size,
        })
    return {"labels": [h.positions[s] for s in ring], "contrib": [contrib[s] for s in ring],
            "pot": pre[0].pot_before, "actions": acts}, [o for o in obs]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", required=True)
    ap.add_argument("--platform", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--every", type=int, default=15)
    a = ap.parse_args()
    hands = sorted(db.load_hands(a.db, a.platform), key=lambda h: h.hand_id)[:: a.every]
    vectors, all_obs = [], []
    for h in hands:
        got = hand_vector(h)
        if got is None:
            continue
        vectors.append(got[0])
        all_obs.extend(got[1])
    model = pool_model.fit(all_obs)
    model["shown"] = {}
    cells = {}
    for v in vectors:
        for act in v["actions"]:
            key = (act["family"], tuple(sorted(act["dims"].items())))
            if key not in cells:
                cells[key] = {"family": act["family"], "dims": act["dims"],
                              **pool_model.predict(model, act["family"], act["dims"])}
    os.makedirs(a.out, exist_ok=True)
    pool_model.save(model, os.path.join(a.out, "pool_model_small.json"))
    with open(os.path.join(a.out, "pool_model_preflop_vectors.json"), "w") as f:
        json.dump({"hands": vectors, "predictions": list(cells.values())}, f, ensure_ascii=False,
                  separators=(",", ":"), sort_keys=True)
        f.write("\n")
    n_act = sum(len(v["actions"]) for v in vectors)
    print(f"{len(vectors)} hands, {n_act} preflop decisions, {len(cells)} cells")
    return 0


if __name__ == "__main__":
    sys.exit(main())

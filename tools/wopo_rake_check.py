#!/usr/bin/env python3
"""Check the rake rule used by `TableConfig::rake_bps` / `rake_cap` against real wopo hands.

wopo deducts rake after `hand_result`, so `final_stacks` does not include it; the deduction shows up
as the difference between a winner's final stack and his starting stack in the next hand. This script
reads driver hand logs (`wopo_hh.jsonl` only, no server API), takes every pair of consecutive hands
in one file and compares what each winner lost between hands with the rule:

    rake = min(floor(4% x total_pot), 10 BB), charged to a seat that wins the pot alone;
    a pot that is split pays nothing; an uncalled bet is not part of total_pot.

Winners of all-in showdowns are sometimes charged more than the rule: that is the premium of all-in
insurance, an optional side bet outside the hand (confirmed by the user 2026-11-11), so it is reported
separately and is not part of the rake rule.

Usage (repo root):  python3 tools/wopo_rake_check.py [glob ...]
Default glob: artifacts/wopo_ante200_*_live_2026*/wopo_hh.jsonl
"""

from __future__ import annotations

import glob
import json
import sys
from collections import Counter

RAKE_BPS = 400
CAP_BB = 10


def rule(total_pot: int, bb: int, unit: int) -> int:
    """Rake on a pot won alone, rounded down to the table's smallest chip `unit`."""
    return min(total_pot * RAKE_BPS // 10_000 // unit * unit, CAP_BB * bb)


def chip_unit(rec: dict) -> int:
    """Smallest chip on the table: 100 when every stack and blind is a multiple of 100, else 1."""
    vals = list(rec["initial_stacks"].values()) + [rec["small_blind"], rec["big_blind"], rec["ante"]]
    return 100 if all(v % 100 == 0 for v in vals) else 1


def load(paths: list[str]) -> list[list[dict]]:
    seen: set[str] = set()
    files = []
    for path in sorted(paths):
        rows = []
        with open(path) as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                rec = json.loads(line)
                if rec["hand_id"] in seen:  # the same hand logged by two of our connections
                    continue
                seen.add(rec["hand_id"])
                rows.append(rec)
        files.append(rows)
    return files


def main() -> int:
    patterns = sys.argv[1:] or ["artifacts/wopo_ante200_*_live_2026*/wopo_hh.jsonl"]
    paths = [p for pat in patterns for p in glob.glob(pat)]
    if not paths:
        print("no hand logs matched", file=sys.stderr)
        return 2
    kinds: Counter = Counter()
    extra_bb = rule_bb = 0.0
    pairs = 0
    for rows in load(paths):
        for a, b in zip(rows, rows[1:]):
            na, nb = (int(r["hand_id"].rsplit(":", 1)[-1]) for r in (a, b))
            if nb != na + 1 or a["big_blind"] != b["big_blind"]:
                continue
            res = a["result"]
            init, final = a["initial_stacks"], res["final_stacks"]
            total_pot = res.get("total_pot")
            if total_pot is None:
                continue
            # Seats seen in both hands under the same name: stack lost between the two hands.
            lost = {
                s: final[s] - b["initial_stacks"][s]
                for s in init
                if s in final and s in b["initial_stacks"] and a["names"].get(s) == b["names"].get(s)
            }
            winners = [s for s in init if s in final and final[s] > init[s]]
            if not winners or any(w not in lost for w in winners):
                continue
            # A non-winner who lost chips between hands means something else happened (hidden hand).
            if any(v > 0 for s, v in lost.items() if s not in winners):
                kinds["skipped: a non-winner lost chips between hands"] += 1
                continue
            pairs += 1
            bb = a["big_blind"]
            all_in = any(x.get("action") == "all_in" or x.get("stack_before") == x.get("contribution_delta")
                         for x in a["actions"] if x.get("contribution_delta"))
            if len(winners) > 1:
                payouts = {str(p["seat"]): p["amount"] for p in res.get("payouts", [])}
                tie = len(set(payouts.get(w) for w in winners)) == 1
                paid = sum(lost[w] for w in winners)
                if tie:
                    kinds["split pot: no rake" if paid in (0, 1) else "split pot: other"] += 1
                else:
                    ok = all(lost[w] == rule(payouts.get(w, 0), bb, chip_unit(a)) for w in winners)
                    kinds["side pots, different winners: each pays own" if ok else "side pots: other"] += 1
                continue
            got = lost[winners[0]]
            want = rule(total_pot, bb, chip_unit(a))
            rule_bb += want / bb
            if got == want:
                kinds["single winner: exact"] += 1
            elif abs(got - want) <= 2:
                kinds["single winner: off by 1-2 chips"] += 1
            elif got > want:
                kinds["single winner: more than the rule" + (" (all-in pot)" if all_in else "")] += 1
                extra_bb += (got - want) / bb
            else:
                kinds["single winner: less than the rule (top-up between hands)"] += 1
    print(f"consecutive hand pairs checked: {pairs}")
    for k, v in sorted(kinds.items(), key=lambda kv: -kv[1]):
        print(f"  {v:5d}  {k}")
    print(f"rake by the rule: {rule_bb:.0f} BB; extra deductions (all-in insurance premiums): {extra_bb:.0f} BB")
    return 0


if __name__ == "__main__":
    sys.exit(main())

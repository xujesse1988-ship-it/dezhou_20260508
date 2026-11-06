#!/usr/bin/env python3
"""Cross-check the arena engine's chips against PokerKit on random hands.

Random stacks and an aggressive random policy produce side pots, short all-ins and refunds. Each hand is
replayed through tools/pokerkit_replay.py's PokerKit path (same action order, same cards) and every seat's
net must match. Split pots may differ by odd chips, because the two engines hand those out differently.

Needs pokerkit importable, e.g. PYTHONPATH=.venv-pokerkit/lib/python3.11/site-packages.
"""

import argparse
import collections
import os
import random
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.dirname(HERE))

import pokerkit  # noqa: E402
from pokerkit_replay import _replay_with_pokerkit  # noqa: E402

from arena import DECK  # noqa: E402
from engine import Hand  # noqa: E402


def random_hand(rng, n):
    deck = list(DECK)
    rng.shuffle(deck)
    holes = [deck[i * 2:i * 2 + 2] for i in range(n)]
    board = deck[n * 2:n * 2 + 5]
    bb, sb, ante = 100, 50, 100
    stacks = [rng.choice((rng.randint(300, 1500), rng.randint(1500, 8000), rng.randint(8000, 40000))) for _ in range(n)]
    button = rng.randrange(n)
    # Antes and blinds must leave chips: the table's stack rules guarantee that too.
    stacks = [max(s, ante + bb + 1) for s in stacks]
    h = Hand(stacks, button, sb, bb, ante, holes, board)
    actions = []
    while not (h.over_by_fold() or h.needs_showdown()):
        if h.round_closed():
            h.next_street()
            continue
        legal = h.legal()
        roll = rng.random()
        # PokerKit refuses a raise that no opponent could match any part of. The table and the driver allow
        # it, as the real table's rules are unknown there, and the excess simply comes back as a refund.
        # Skip those here: with the rest of the line unchanged, PokerKit can still check the chips.
        can_raise = legal["can_raise"] and any(
            x != h.to_act and x not in h.folded and h.stack[x] + h.bet[x] > h.max_bet for x in range(n))
        if can_raise and roll < 0.12:
            kind, to = "all_in", None
        elif can_raise and roll < 0.24:
            kind, to = "raise", legal["min_raise"]
        elif can_raise and roll < 0.36:
            kind, to = "raise", rng.randint(legal["min_raise"], legal["max_raise"])
        elif legal["can_check"]:
            kind, to = "check", None
        elif roll < 0.75:
            kind, to = "call", None
        else:
            kind, to = "fold", None
        act = h.apply(kind, to)
        rec = {"street": act["street"], "seat": act["seat"]}
        if act["label"] in ("raise", "all_in"):
            rec.update(kind="raise", to=act["to"])
        else:
            rec["kind"] = act["label"]
        actions.append(rec)
    return h, {"n_seats": n, "starting_stacks": stacks, "small_blind": sb, "big_blind": bb, "ante": ante,
               "button_seat": button, "hole_cards": holes, "board": board, "actions": actions}


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--hands", type=int, default=5000)
    p.add_argument("--seed", type=int, default=1)
    args = p.parse_args()
    rng = random.Random(args.seed)
    tally = collections.Counter()
    failures = 0
    for i in range(args.hands):
        h, req = random_hand(rng, rng.choice((3, 6, 8, 8, 8, 9)))
        res = h.settle()
        ref = _replay_with_pokerkit(req, pokerkit)
        if not ref.get("ok"):
            failures += 1
            print(f"#{i} PokerKit disagreed on the line: {ref}\n  {req}")
            continue
        got = {x["seat"]: x["net"] for x in ref["final_payouts"]}
        diff = max(abs(got[s] - res["net"][s]) for s in range(h.n))
        multi = len(res["winners"]) > 1
        tally["hands"] += 1
        tally["multi_winner"] += multi
        tally["showdown"] += bool(res["showdown"])
        tally["refund"] += any(res["refund"])
        tally["short_call"] += any(a["kind"] == "call" for a in req["actions"]) and any(s == 0 for s in h.stack)
        if diff == 0:
            tally["exact"] += 1
        elif multi and diff <= h.n:
            tally["odd_chip_only"] += 1
        else:
            failures += 1
            print(f"#{i} nets differ by {diff}: arena {res['net']} pokerkit {got}\n  {req}")
    print(dict(tally), "failures", failures)
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()

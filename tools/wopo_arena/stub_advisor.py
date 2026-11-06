#!/usr/bin/env python3
"""Random legal-action advisor for arena tests. Speaks the openpoker_advisor JSON-lines protocol.

Aggressive on purpose: frequent all-ins and min-raises produce the side pots, short all-ins and
refunds that a test of the table needs. Never used in a real match.
"""

import json
import random
import sys


def flag(name, default):
    return sys.argv[sys.argv.index(name) + 1] if name in sys.argv else default


def choose(rng, valid):
    roll = rng.random()
    if valid.get("can_raise"):
        lo, hi = valid["min_raise"], valid["max_raise"]
        if roll < 0.10:
            return {"action": "all_in"}
        if roll < 0.20:
            return {"action": "raise", "amount": lo}
        if roll < 0.32:
            return {"action": "raise", "amount": rng.randint(lo, hi)}
    if valid.get("can_check"):
        return {"action": "check"}
    if roll < 0.75 and valid.get("can_call"):
        return {"action": "call"}
    return {"action": "fold"}


def main():
    rng = random.Random(int(flag("--seed", "1")))
    print(json.dumps({"ready": True, "reshape": "stub", "n_seats": int(flag("--seats", "6")),
                      "ante": int(flag("--ante", "0")), "stack_bb": int(flag("--stack-bb", "100")),
                      "update_count": 0, "search": True}), flush=True)
    for line in sys.stdin:
        req = json.loads(line)
        if req.get("prewarm") or req.get("observe"):
            resp = {"action": "none", "source": "stub:ack"}
        else:
            resp = {**choose(rng, req["valid"]), "source": "stub", "street": "stub"}
        print(json.dumps(resp), flush=True)


if __name__ == "__main__":
    main()

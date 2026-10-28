"""wopo: hh rows as tools/wopo_play.py --hh-log writes them ingest cleanly.

data/wopo_hh_2026_10_16.jsonl is the driver's output for its captured-session fixture; the
driver's --selftest checks it still produces exactly these rows (this package imports no
tools/ code)."""

import json
import os
import tempfile
import unittest

from opponent_analysis import db
from opponent_analysis.platforms import detect_platform
from opponent_analysis.report import Config, analyze

FIXTURE = os.path.join(os.path.dirname(__file__), "data", "wopo_hh_2026_10_16.jsonl")
# Same session rewritten with a 2-chip ante per dealt seat (blinds 1/2); the driver's --selftest
# produces it and checks it is still byte-identical.
ANTE_FIXTURE = os.path.join(os.path.dirname(__file__), "data", "wopo_hh_ante_2026_10_27.jsonl")
# Four live ante-table hands (8 seats, 50/100, ante 100) where a new / returning player posted an
# extra live big blind; the driver's --selftest produces it from tools/testdata/wopo_posts_*.jsonl.
POSTS_FIXTURE = os.path.join(os.path.dirname(__file__), "data", "wopo_hh_posts_2026_10_28.jsonl")


def wopo_hh_rows(path=FIXTURE):
    with open(path) as f:
        return [json.loads(line) for line in f if line.strip()]


class Wopo(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.db = os.path.join(self.tmp.name, "db")
        self.hh = os.path.join(self.tmp.name, "wopo_hh.jsonl")

    def tearDown(self):
        self.tmp.cleanup()

    def ingest(self, rows):
        with open(self.hh, "w") as f:
            for r in rows:
                f.write(json.dumps(r) + "\n")
        return db.ingest(self.db, self.hh)

    def test_driver_log_ingests(self):
        rows = wopo_hh_rows()
        self.assertEqual(detect_platform(rows[0]), "wopo")
        e = self.ingest(rows)
        self.assertEqual(e["platform"], "wopo")
        # wopo marks no timeout actions, so every hand is noted forced_unknown.
        self.assertEqual(e["counts"], {"accepted": 7, "forced_unknown": 7})
        hands = db.load_hands(self.db, "wopo")
        self.assertEqual(len(hands), 7)
        h7 = next(h for h in hands if h.hand_id.endswith("000007"))
        # Seat 3 bets 37 on the river, both fold: the uncalled bet comes back, net +50.
        self.assertEqual(h7.final_stacks[3] - h7.start_stacks[3], 50)
        self.assertEqual(h7.names, {0: "hero", 1: "Shaggyx", 2: "hsjwnbxj", 3: "Jjdjhej"})
        res = analyze(hands, "wopo", [], Config(min_hands=1, min_opps=1))
        self.assertEqual(res["hands"], 7)

    def test_ante_hands_ingest(self):
        rows = wopo_hh_rows(ANTE_FIXTURE)
        self.assertTrue(all(r["ante"] == 2 for r in rows))
        e = self.ingest(rows)
        self.assertEqual(e["counts"], {"accepted": 7, "forced_unknown": 7})
        hands = {h.hand_id: h for h in db.load_hands(self.db, "wopo")}
        base_db = os.path.join(self.tmp.name, "base_db")
        base_hh = os.path.join(self.tmp.name, "base.jsonl")
        with open(base_hh, "w") as f:
            for r in wopo_hh_rows():
                f.write(json.dumps(r) + "\n")
        db.ingest(base_db, base_hh)
        for b in db.load_hands(base_db, "wopo"):
            h = hands[b.hand_id]
            # The ante is dead money: every dealt seat commits 2 more, decisions are unchanged.
            self.assertEqual(h.committed, {s: c + 2 for s, c in b.committed.items()})
            self.assertEqual(len(h.decisions), len(b.decisions))
            for s in h.folded:
                self.assertEqual(h.final_stacks[s] - h.start_stacks[s], -h.committed[s])
            for d, bd in zip(h.decisions, b.decisions):
                self.assertEqual(d.pot_before, bd.pot_before + 2 * len(h.dealt_seats))

    def test_live_post_hands_ingest(self):
        rows = wopo_hh_rows(POSTS_FIXTURE)
        self.assertEqual([r["posts"] for r in rows], [[6], [2], [7], [4]])
        e = self.ingest(rows)
        self.assertEqual(e["counts"], {"accepted": 4, "forced_unknown": 4})
        hands = {h.hand_id[-3:]: h for h in db.load_hands(self.db, "wopo")}
        h = hands["132"]
        # Seat 4 posted 100 on top of the 100 ante and folded to the open: both are dead money.
        self.assertEqual(h.committed[4], 200)
        self.assertEqual(h.final_stacks[4] - h.start_stacks[4], -200)
        # The post is a live bet: the first decision after it sees the pot with it already in.
        first = h.decisions[0]
        self.assertEqual(first.pot_before, 8 * 100 + 50 + 100 + 100)
        self.assertEqual(first.to_call, 100)

    def test_live_follows_wopo_log(self):
        from opponent_analysis.live import Live

        rows = wopo_hh_rows()
        with open(self.hh, "w") as f:
            for r in rows[:3]:
                f.write(json.dumps(r) + "\n")
        lv = Live(self.db, "wopo", [self.hh], Config(min_hands=1, min_opps=1), refresh_s=0)
        self.addCleanup(lv.close)
        lv.bootstrap()
        self.assertEqual(lv.snapshot["hands"], 3)
        self.assertEqual(lv.snapshot["hero_names"], ["hero"])
        with open(self.hh, "a") as f:
            for r in rows[3:]:
                f.write(json.dumps(r) + "\n")
        self.assertEqual(lv.step(), 4)
        lv.build()
        self.assertEqual(lv.snapshot["hands"], 7)

    def test_bad_final_stacks_rejected(self):
        rows = wopo_hh_rows()
        rows[0]["result"] = dict(rows[0]["result"], final_stacks={"0": 200, "1": 175, "2": 175, "3": 251})
        e = self.ingest(rows)
        self.assertEqual(e["counts"], {"accepted": 6, "forced_unknown": 6, "rejected:not_zero_sum": 1})

    def test_heads_up_rejected(self):
        rows = wopo_hh_rows()
        rows[0]["dealt_seats"] = [0, 1]
        e = self.ingest(rows)
        self.assertEqual(e["counts"], {"accepted": 6, "forced_unknown": 6,
                                       "rejected:heads_up_unsupported": 1})


if __name__ == "__main__":
    unittest.main()

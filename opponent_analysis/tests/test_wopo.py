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


def wopo_hh_rows():
    with open(FIXTURE) as f:
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

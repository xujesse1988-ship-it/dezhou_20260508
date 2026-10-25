import json
import os
import tempfile
import unittest
from unittest import mock

from opponent_analysis import db
from opponent_analysis.report import Config, analyze
from opponent_analysis.tests.helpers import pm_record

STACKS = {s: 1000 for s in range(3)}


def river_showdown(hid, shown, board=("2c", "3d", "4h", "8s", "9c")):
    """BTN opens, BB calls, checked down to showdown."""
    return pm_record(hid, 0, [0, 1, 2], STACKS,
                     [(0, "raise", 60), (1, "fold", None), (2, "call", 40)] + [(2, "check", None), (0, "check", None)] * 3,
                     {0: 1070, 1: 990, 2: 940}, board=list(board), shown=shown)


class Store(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = self.tmp.name
        self.hh = os.path.join(self.dir, "x_hh.jsonl")
        recs = [
            pm_record("a", 0, [0, 1, 2], {s: 1000 for s in range(3)}, [(0, "fold", None), (1, "fold", None)],
                      {0: 1000, 1: 990, 2: 1010}),
            pm_record("b", 0, [0, 1, 2], {s: 1000 for s in range(3)}, [(0, "fold", None)], {0: 1000, 1: 990, 2: 1010}),
        ]
        with open(self.hh, "w") as f:
            for r in recs:
                f.write(json.dumps(r) + "\n")

    def tearDown(self):
        self.tmp.cleanup()

    def test_ingest_dedupes_and_records_rejects(self):
        e = db.ingest(os.path.join(self.dir, "db"), self.hh)
        self.assertEqual(e["counts"], {"accepted": 1, "rejected:incomplete": 1})
        e = db.ingest(os.path.join(self.dir, "db"), self.hh)
        self.assertEqual(e["counts"], {"duplicate": 1, "duplicate_rejected": 1})
        self.assertEqual(len(db.load_hands(os.path.join(self.dir, "db"), "poker_manager")), 1)
        self.assertEqual(db.load_hands(os.path.join(self.dir, "db"), "openpoker"), [])

    def test_one_writer_per_database(self):
        d = os.path.join(self.dir, "db")
        with db.Store(d, "poker_manager"):
            with self.assertRaises(db.DatabaseBusy):
                db.ingest(d, self.hh)
            db.Store(d, "openpoker").close()  # another platform is another directory
        db.ingest(d, self.hh)  # released on close

    def test_load_drops_duplicate_hand_ids(self):
        d = os.path.join(self.dir, "db")
        db.ingest(d, self.hh)
        for name in ("hands.jsonl", "rejected.jsonl"):  # as two concurrent writers used to leave them
            path = os.path.join(d, "poker_manager", name)
            with open(path) as f:
                line = f.readline()
            with open(path, "a") as f:
                f.write(line)
        self.assertEqual([h.hand_id for h in db.load_hands(d, "poker_manager")], ["a"])
        self.assertEqual(len(db.load_rejected(d, "poker_manager")), 1)

    def test_bad_records_are_rejected_not_raised(self):
        d = os.path.join(self.dir, "db")
        odd = pm_record("odd", 0, [0, 1, 2], STACKS, [(0, "fold", None), (1, "fold", None)], {0: 1000, 1: 990, 2: 1010})
        odd["result"] = ["not", "a", "dict"]
        with db.Store(d, "poker_manager") as st:
            self.assertEqual(st.add(odd, "t")[0], "rejected:malformed")
            self.assertEqual(st.add(river_showdown("bad", {0: ["Xx", "Kd"], 2: ["Ah", "Kh"]}), "t")[0], "rejected:bad_cards")
            self.assertEqual(st.add(river_showdown("dup", {0: ["2c", "Kd"], 2: ["Ah", "Kh"]}), "t")[0], "rejected:bad_cards")
            self.assertEqual(st.add(river_showdown("one", {0: ["Qc"], 2: ["Ah", "Kh"]}), "t")[0], "rejected:bad_cards")
            self.assertEqual(st.add(river_showdown("ok", {0: ["Qc", "Qd"], 2: ["Ah", "Kh"]}), "t")[0], "accepted")
            with mock.patch("opponent_analysis.db.hand_observations", side_effect=RuntimeError("boom")):
                self.assertEqual(st.add(river_showdown("boom", {}), "t")[0], "rejected:spots_error")
        self.assertEqual([h.hand_id for h in db.load_hands(d, "poker_manager")], ["ok"])

    def test_bb_short_of_small_blind_does_not_break_the_report(self):
        # BB has 5 chips and posts them all-in; the SB already has 10 in and checks.
        rec = pm_record("x1", 0, [0, 1, 2], {0: 1000, 1: 1000, 2: 5}, [(0, "fold", None), (1, "check", None)],
                        {0: 1000, 1: 995, 2: 10}, board=["2c", "3d", "4h", "8s", "9c"])
        d = os.path.join(self.dir, "db")
        with db.Store(d, "poker_manager") as st:
            self.assertEqual(st.add(rec, "t")[0], "accepted")
        res = analyze(db.load_hands(d, "poker_manager"), "poker_manager", [], Config())
        self.assertEqual(res["hands"], 1)

    def test_platform_mismatch_is_refused(self):
        with self.assertRaises(ValueError):
            db.ingest(os.path.join(self.dir, "db"), self.hh, platform="openpoker")


if __name__ == "__main__":
    unittest.main()

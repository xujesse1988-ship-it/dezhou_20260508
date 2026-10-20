import json
import os
import tempfile
import unittest

from opponent_analysis import db
from opponent_analysis.tests.helpers import pm_record


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

    def test_platform_mismatch_is_refused(self):
        with self.assertRaises(ValueError):
            db.ingest(os.path.join(self.dir, "db"), self.hh, platform="openpoker")


if __name__ == "__main__":
    unittest.main()

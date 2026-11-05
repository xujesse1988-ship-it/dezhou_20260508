"""--aliases: several names of one person share one profile; the database keeps raw names; report and
live agree; the page gets the raw -> profile name map. Same rules as tools/poker_live/aliases.py, whose
table (tools/poker_live/wopo_aliases.json) must load here too."""

import json
import os
import tempfile
import unittest

from opponent_analysis import aliases as al
from opponent_analysis import db
from opponent_analysis.report import Config, analyze
from opponent_analysis.tests.test_wopo import FIXTURE, wopo_hh_rows

REPO_TABLE = os.path.join(os.path.dirname(__file__), "..", "..", "tools", "poker_live", "wopo_aliases.json")


class Rules(unittest.TestCase):
    def test_canon_and_hand_clashes(self):
        a = al.Aliases({"ab...": "abcd"}, [(r"[0-9]h9$", "h9组")])
        self.assertEqual([a.canon(n) for n in ("x3h9", "ab...", "bob", "h9x")], ["h9组", "abcd", "bob", "h9x"])
        self.assertEqual(a.names({0: "hero", 1: "x3h9", 2: "bob"}, 0), {0: "hero", 1: "h9组", 2: "bob"})
        # Two seats -> one alias, or alias == another seat's raw name: those seats keep raw names.
        self.assertEqual(a.names({1: "x3h9", 2: "y2h9"}), {1: "x3h9", 2: "y2h9"})
        self.assertEqual(a.names({1: "ab...", 2: "abcd"}), {1: "ab...", 2: "abcd"})
        self.assertEqual(a.names({1: "p1h9", 2: "h9组"}), {1: "p1h9", 2: "h9组"})
        self.assertEqual(a.names({0: "z5h9", 1: "z6h9"}, 0), {0: "z5h9", 1: "h9组"})  # hero seat never renamed
        self.assertEqual(a.seen, {"x3h9": "h9组", "z6h9": "h9组"})

    def test_repo_table_loads(self):
        a = al.load(REPO_TABLE)
        self.assertEqual(a.canon("嵩域鎏锋3h9"), "h9组")
        self.assertEqual(a.canon("TRGFDGD..."), "TRGFDGDHG")
        self.assertEqual(a.canon("hero"), "hero")
        self.assertFalse(al.load(None))
        self.assertFalse(al.load("off"))

    def test_bad_tables_raise(self):
        with tempfile.TemporaryDirectory() as d:
            for bad in ('[]', '{"patterns": [{"regex": "("}]}', '{"patterns": [{"regex": "x", "alias": ""}]}',
                        '{"exact": {"a": ""}}', '{"exact": []}'):
                path = os.path.join(d, "bad.json")
                with open(path, "w") as f:
                    f.write(bad)
                with self.assertRaises(al.AliasError, msg=bad):
                    al.load(path)


class Merged(unittest.TestCase):
    """The fixture session (seats 1-3: Shaggyx, hsjwnbxj, Jjdjhej) with Shaggyx and hsjwnbxj declared one person.
    They sit together in every hand, so the clash rule keeps them apart; Jjdjhej -> J merges cleanly."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.db = os.path.join(self.tmp.name, "db")
        self.hh = os.path.join(self.tmp.name, "wopo_hh.jsonl")
        with open(self.hh, "w") as f:
            for r in wopo_hh_rows():
                f.write(json.dumps(r) + "\n")
        db.ingest(self.db, self.hh, None, "wopo")

    def tearDown(self):
        self.tmp.cleanup()

    def table(self):
        return al.Aliases({"Jjdjhej": "J", "Shaggyx": "S", "hsjwnbxj": "S"})

    def test_report_and_live_agree(self):
        from opponent_analysis.live import Live

        raw = db.load_hands(self.db, "wopo")
        a = self.table()
        res = analyze([a.hand(h) for h in raw], "wopo", [], Config(min_hands=1, min_opps=1))
        self.assertEqual(sorted(res["players"]), ["J", "Shaggyx", "hero", "hsjwnbxj"])
        base = analyze(raw, "wopo", [], Config(min_hands=1, min_opps=1))
        self.assertEqual(res["players"]["J"].hands, base["players"]["Jjdjhej"].hands)
        self.assertEqual(round(res["players"]["J"].net_bb, 6), round(base["players"]["Jjdjhej"].net_bb, 6))
        # The database is untouched.
        self.assertTrue(all("Jjdjhej" in h.names.values() for h in db.load_hands(self.db, "wopo")))

        lv = Live(self.db, "wopo", [self.hh], Config(min_hands=1, min_opps=1), refresh_s=0, aliases=self.table())
        self.addCleanup(lv.close)
        lv.bootstrap()
        s = lv.snapshot
        self.assertEqual(sorted(p["name"] for p in s["players"]), sorted(res["players"]))
        self.assertEqual(s["aliases"], {"Jjdjhej": "J"})
        self.assertTrue(all("J" in {x["name"] for x in t["seats"]} for t in s["tables"]))

    def test_no_table_is_unchanged(self):
        from opponent_analysis.live import Live

        lv = Live(self.db, "wopo", [self.hh], Config(min_hands=1, min_opps=1), refresh_s=0)
        self.addCleanup(lv.close)
        lv.bootstrap()
        self.assertEqual(lv.snapshot["aliases"], {})
        self.assertIn("Jjdjhej", {p["name"] for p in lv.snapshot["players"]})


if __name__ == "__main__":
    unittest.main()

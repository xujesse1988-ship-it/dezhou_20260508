import json
import os
import random
import tempfile
import unittest
from unittest import mock

from opponent_analysis import db
from opponent_analysis.live import Follower, Live
from opponent_analysis.report import Config, analyze, profile_json, render_markdown
from opponent_analysis.tests.helpers import pm_record

STACKS = {s: 1000 for s in range(3)}


def walk(hid, names, ts):
    """Folds to the big blind."""
    r = pm_record(hid, 0, [0, 1, 2], STACKS, [(0, "fold", None), (1, "fold", None)],
                  {0: 1000, 1: 990, 2: 1010}, names=names)
    r["result"]["ts"] = ts
    r["result"]["winners"] = [{"seat": 2, "name": names[2]}]  # the BB never acts; its name comes from here
    return r


def raise_cbet(hid, names, ts):
    """BTN opens to 60, SB folds, BB calls; flop BB checks, BTN bets 60, BB folds."""
    r = pm_record(hid, 0, [0, 1, 2], STACKS,
                  [(0, "raise", 60), (1, "fold", None), (2, "call", 40), (2, "check", None), (0, "bet", 60),
                   (2, "fold", None)],
                  {0: 1070, 1: 990, 2: 940}, board=["2c", "7d", "Kh"], names=names)
    r["result"]["ts"] = ts
    return r


def broken(hid, names, ts):
    """Betting stops while the BB still has to act -> rejected as incomplete."""
    r = pm_record(hid, 0, [0, 1, 2], STACKS, [(0, "fold", None)], {0: 1000, 1: 990, 2: 1010}, names=names)
    r["result"]["ts"] = ts
    return r


def iso(i):
    return f"2026-10-25T00:{i // 60:02d}:{i % 60:02d}+00:00"


class FollowerTest(unittest.TestCase):
    def test_partial_lines_new_files_and_rewrites(self):
        with tempfile.TemporaryDirectory() as d:
            a = os.path.join(d, "run_a", "x_hh.jsonl")
            os.makedirs(os.path.dirname(a))
            with open(a, "w") as f:
                f.write('{"n": 1}\n{"n": ')
            fo = Follower([os.path.join(d, "run_*", "x_hh.jsonl")])
            self.assertEqual([r for _, _, r in fo.poll()], [{"n": 1}])
            self.assertEqual(fo.poll(), [])  # the half line waits for its newline
            with open(a, "a") as f:
                f.write('2}\nnot json\n')
            got = fo.poll()
            self.assertEqual([(ln, r) for _, ln, r in got], [(2, {"n": 2})])
            self.assertEqual(fo.files[a]["bad_lines"], 1)
            b = os.path.join(d, "run_b", "x_hh.jsonl")
            os.makedirs(os.path.dirname(b))
            with open(b, "w") as f:
                f.write('{"n": 3}\n')
            self.assertEqual([(p, r) for p, _, r in fo.poll()], [(b, {"n": 3})])
            with open(a, "w") as f:  # rewritten shorter: read again from the start
                f.write('{"n": 4}\n')
            self.assertEqual([(p, ln, r) for p, ln, r in fo.poll()], [(a, 1, {"n": 4})])


class LiveTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = self.tmp.name
        self.db = os.path.join(self.dir, "db")
        self.hh = os.path.join(self.dir, "run", "poker_manager_hh.jsonl")
        os.makedirs(os.path.dirname(self.hh))
        open(self.hh, "w").close()
        self.pattern = os.path.join(self.dir, "run*", "*_hh.jsonl")

    def tearDown(self):
        self.tmp.cleanup()

    def append(self, path, recs):
        with open(path, "a") as f:
            for r in recs:
                f.write(json.dumps(r) + "\n")

    def live(self, **kw):
        lv = Live(self.db, "poker_manager", [self.pattern], Config(min_hands=5, min_opps=3), refresh_s=0, **kw)
        self.addCleanup(lv.close)
        lv.bootstrap()
        return lv

    def test_follow_store_and_resume(self):
        names = {0: "hero", 1: "v1", 2: "v2"}
        self.append(self.hh, [walk("a", names, iso(1)), broken("b", names, iso(2))])
        lv = self.live()
        s = lv.snapshot
        self.assertEqual((s["hands"], s["rejected"], s["version"]), (1, {"incomplete": 1}, 1))
        self.assertEqual(s["hero_names"], ["hero"])
        self.assertEqual({p["name"]: p["hero"] for p in s["players"]}, {"hero": True, "v1": False, "v2": False})

        self.append(self.hh, [raise_cbet("c", names, iso(3))])
        self.assertEqual(lv.step(), 1)
        lv.build()
        s = lv.snapshot
        self.assertEqual((s["hands"], s["version"]), (2, 2))
        self.assertEqual(s["recent"][0]["hand_id"], "c")
        self.assertEqual(s["tables"][0]["hand_id"], "c")
        v2 = next(p for p in s["players"] if p["name"] == "v2")
        self.assertEqual(v2["net_bb"], 0.5 - 3.0)
        self.assertEqual(lv.step(), 0)

        # Restart: everything comes back from the database; the followed file only dedupes.
        with self.assertRaises(db.DatabaseBusy):  # one writer per database
            self.live()
        lv.close()
        lv2 = self.live()
        self.assertEqual(lv2.snapshot["hands"], 2)
        self.assertEqual(lv2.counts, {"duplicate": 2, "duplicate_rejected": 1})
        self.assertEqual(len(db.load_hands(self.db, "poker_manager")), 2)

    def test_extra_hero_names_are_not_opponents(self):
        self.append(self.hh, [walk("a", {0: "hero", 1: "bot2", 2: "v2"}, iso(1))])
        lv = self.live(hero_names=("bot2",))
        s = lv.snapshot
        self.assertEqual(sorted(s["hero_names"]), ["bot2", "hero"])
        self.assertTrue(next(p for p in s["players"] if p["name"] == "bot2")["hero"])
        self.assertTrue(s["tables"][0]["seats"][1]["hero"])
        # The offline report takes the same names and gives the same result.
        res = analyze(db.load_hands(self.db, "poker_manager"), "poker_manager", [], Config(min_hands=5, min_opps=3),
                      hero_names=("bot2",))
        self.assertNotIn("bot2", res["opponents"])
        self.assertEqual(lv.md, render_markdown(res))

    def test_stored_hands_reach_profile_when_a_later_line_fails(self):
        names = {0: "hero", 1: "v1", 2: "v2"}
        lv = self.live()
        self.append(self.hh, [raise_cbet("c1", names, iso(1)), raise_cbet("c2", names, iso(2))])
        real_add = lv.store.add

        def add(rec, *a):
            if rec["hand_id"] == "c2":
                raise OSError("disk full")
            return real_add(rec, *a)

        with mock.patch.object(lv.store, "add", side_effect=add):
            with self.assertRaises(OSError):
                lv.step()
        self.assertEqual(lv.acc.n, 1)  # c1 is in the database, so it is in the profile too
        self.assertEqual(len(db.load_hands(self.db, "poker_manager")), 1)

    def test_live_matches_offline_report(self):
        rng = random.Random(5)
        pool = ["v1", "v2", "v3", "v4"]
        recs = []
        for i in range(60):
            nm = {0: "hero"}
            nm.update(zip((1, 2), rng.sample(pool, 2)))
            recs.append((walk if rng.random() < 0.4 else raise_cbet)(f"h{i}", nm, iso(i)))
        lv = self.live()
        for i in range(0, 60, 7):  # arrive in uneven batches, profile rebuilt after each
            self.append(self.hh, recs[i:i + 7])
            lv.step()
            lv.build()
        res = analyze(db.load_hands(self.db, "poker_manager"), "poker_manager", [], Config(min_hands=5, min_opps=3))
        self.assertEqual(lv.md, render_markdown(res))
        self.assertEqual(lv.profile, profile_json(res))

    def test_older_file_triggers_time_ordered_rebuild(self):
        names = {0: "hero", 1: "v1", 2: "v2"}
        self.append(self.hh, [raise_cbet(f"n{i}", names, iso(30 + i)) for i in range(5)])
        lv = self.live()
        older = os.path.join(self.dir, "run0", "poker_manager_hh.jsonl")
        os.makedirs(os.path.dirname(older))
        self.append(older, [walk(f"o{i}", names, iso(i)) for i in range(5)])
        self.assertEqual(lv.step(), 5)
        lv.build()
        res = analyze(db.load_hands(self.db, "poker_manager"), "poker_manager", [], Config(min_hands=5, min_opps=3))
        self.assertEqual(lv.md, render_markdown(res))
        self.assertEqual([h["hand_id"] for h in lv.snapshot["recent"]][:2], ["n4", "n3"])


if __name__ == "__main__":
    unittest.main()

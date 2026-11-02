"""Hand replay (history): hh rows -> per-step table states, play events attached, served over HTTP."""

import json
import os
import shutil
import tempfile
import time
import unittest

from opponent_analysis.history import History, act_label, build_hand, expand, handle, load_run

DATA = os.path.join(os.path.dirname(__file__), "data")
HH = os.path.join(DATA, "wopo_hh_2026_10_16.jsonl")
EVENTS = os.path.join(DATA, "wopo_play_events_2026_10_16.jsonl")
FIXTURES = [HH, os.path.join(DATA, "wopo_hh_ante_2026_10_27.jsonl"), os.path.join(DATA, "wopo_hh_posts_2026_10_28.jsonl")]


def rows(path):
    with open(path, encoding="utf-8") as f:
        return [json.loads(line) for line in f if line.strip()]


def by_hand(events):
    out = {}
    for e in events:
        out.setdefault(e["hand_id"], []).append(e)
    return out


class BuildHand(unittest.TestCase):
    def test_every_step_conserves_chips_and_matches_the_log(self):
        for path in FIXTURES:
            for r in rows(path):
                with self.subTest(path=os.path.basename(path), hand=r["hand_id"]):
                    h = build_hand(r)
                    # stack_before of every logged action agreed with the re-simulation
                    self.assertEqual(h["notes"], list(r.get("result_notes") or []))
                    self.assertIsNone(h["check"])
                    start = {x["seat"]: x["start"] for x in h["seats"]}
                    seats = [x["seat"] for x in h["seats"]]
                    for st in h["steps"][:-1]:
                        put_in = sum(start[s] - st["seats"][i][0] for i, s in enumerate(seats))
                        self.assertEqual(st["pot"], put_in)
                    acts = [st for st in h["steps"] if st["kind"] == "action"]
                    self.assertEqual([(st["seat"], st["action"]) for st in acts],
                                     [(a["seat"], a["action"]) for a in r["actions"]])
                    for st, a in zip(acts, r["actions"]):
                        self.assertEqual(st["delta"], 0 if a["action"] == "fold" else a["contribution_delta"])
                    end = h["steps"][-1]
                    self.assertEqual(end["kind"], "result")
                    final = {int(k): v for k, v in r["result"]["final_stacks"].items()}
                    self.assertEqual([x[0] for x in end["seats"]], [final[s] for s in seats])
                    self.assertEqual(end["board"], r["board"])
                    boards = [st["board"] for st in h["steps"] if st["kind"] == "board"]
                    self.assertEqual(boards, [r["board"][:n] for n in (3, 4, 5)][:len(boards)])

    def test_blinds_ante_posts_and_labels(self):
        posts = rows(FIXTURES[2])[0]
        h = build_hand(posts)
        st = h["steps"][0]
        dealt = sorted(posts["dealt_seats"])
        ante, bb = posts["ante"], posts["big_blind"]
        for i, s in enumerate(dealt):
            blind = posts["small_blind"] if s == h["sb_seat"] else bb if s == h["bb_seat"] or s in posts["posts"] else 0
            self.assertEqual(st["seats"][i][1], blind, s)  # street bet: blinds and posts, not the ante
            self.assertEqual(st["seats"][i][0], posts["initial_stacks"][str(s)] - ante - blind, s)
        self.assertTrue(any(x[4] and x[4].startswith("补盲") for x in st["seats"]))

    def test_uncalled_bet_is_returned(self):
        h7 = next(r for r in rows(HH) if r["hand_id"].endswith("000007"))
        h = build_hand(h7)
        river = [st for st in h["steps"] if st["kind"] == "action" and st["street"] == "river"]
        self.assertEqual([st["label"] for st in river], ["check", "check", "bet 37", "fold", "fold"])
        end = h["steps"][-1]
        self.assertEqual(end["returned"], 37)
        self.assertEqual(end["pot"], 75)
        seat3 = next(x for x in h["seats"] if x["seat"] == 3)
        self.assertEqual(seat3["net"], 50)

    def test_act_label(self):
        self.assertEqual(act_label("raise", 0, 140, 140, 0, False), "bet 140")
        self.assertEqual(act_label("raise", 20, 100, 100, 20, False), "raise 到 100")
        self.assertEqual(act_label("call", 0, 0, 20, 20, False), "check")
        self.assertEqual(act_label("all_in", 500, 300, 300, 500, True), "call 300（全下）")
        self.assertEqual(act_label("all_in", 20, 900, 900, 20, True), "raise 到 900（全下）")

    def test_hero_decisions_and_timing_from_play_events(self):
        evs = by_hand(rows(EVENTS))
        n_hero = 0
        for r in rows(HH):
            h = build_hand(r, evs.get(r["hand_id"]))
            self.assertTrue(h["has_events"])
            hero_evs = [e for e in evs[r["hand_id"]] if e["type"] == "hero_action"]
            hero_steps = [st for st in h["steps"] if st["kind"] == "action" and st["seat"] == r["my_seat"]]
            self.assertEqual(len(hero_steps), len(hero_evs))
            for st, e in zip(hero_steps, hero_evs):
                n_hero += 1
                self.assertEqual(st["hero"]["match"], e["match"])
                self.assertEqual(st["hero"]["advice"], e["advice"])
                dec = next(d for d in evs[r["hand_id"]] if d["type"] == "decision" and d["turn"] == e["turn"])
                self.assertEqual(st["hero"]["decision"]["source"], dec["source"])
            self.assertTrue(all(st.get("think_s") is not None for st in h["steps"] if st["kind"] == "action"))
        self.assertGreater(n_hero, 0)

    def test_events_that_disagree_are_not_attached(self):
        r = rows(HH)[0]
        evs = [dict(e) for e in rows(EVENTS) if e["hand_id"] == r["hand_id"]]
        first = next(e for e in evs if e["type"] in ("action", "hero_action"))
        first["seat"] = 99
        h = build_hand(r, evs)
        self.assertTrue(any("对不上" in n for n in h["notes"]))
        self.assertFalse(any(st.get("hero") or st.get("think_s") for st in h["steps"]))

    def test_failed_validation_is_a_warning_not_a_drop(self):
        r = json.loads(json.dumps(rows(HH)[0]))
        fs = r["result"]["final_stacks"]
        fs["0"] += 5  # not zero-sum, like wopo's extra all-in deductions
        h = build_hand(r)
        self.assertIsNotNone(h["check"])
        self.assertEqual(h["steps"][-1]["kind"], "result")


class Files(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.run = os.path.join(self.tmp.name, "wopo_run_a")
        os.makedirs(self.run)
        shutil.copy(HH, os.path.join(self.run, "wopo_hh.jsonl"))
        shutil.copy(EVENTS, os.path.join(self.run, "wopo_play_events.jsonl"))

    def tearDown(self):
        self.tmp.cleanup()

    def test_load_run_counts_dead_hands(self):
        r = load_run(os.path.join(self.run, "wopo_hh.jsonl"))
        self.assertEqual(r["counts"]["hands"], 7)
        hh_ids = {x["hand_id"] for x in rows(HH)}
        ev_ids = {e["hand_id"] for e in rows(EVENTS) if e["type"] == "hand_start"}
        self.assertEqual(set(r["dead"]), ev_ids - hh_ids)

    def test_expand_dirs_files_globs(self):
        hh = os.path.normpath(os.path.join(self.run, "wopo_hh.jsonl"))
        self.assertEqual(expand([self.run]), [hh])
        self.assertEqual(expand([os.path.join(self.tmp.name, "wopo_*") + "/"]), [hh])
        self.assertEqual(expand([hh, self.run]), [hh])
        self.assertEqual(expand([os.path.join(self.tmp.name, "nothing_*")]), [])

    def test_reload_on_change_and_key_collision(self):
        hist = History([os.path.join(self.tmp.name, "wopo_*")])
        self.assertEqual(len(hist.hands()["hands"]), 7)
        # A second run directory with the same hand ids: keys stay unique.
        other = os.path.join(self.tmp.name, "wopo_run_b")
        os.makedirs(other)
        shutil.copy(HH, os.path.join(other, "wopo_hh.jsonl"))
        lst = hist.hands()["hands"]
        self.assertEqual(len(lst), 14)
        self.assertEqual(len({x["key"] for x in lst}), 14)
        dup = next(x for x in lst if "@" in x["key"])
        self.assertEqual(hist.hand(dup["key"])["run"], dup["run"])
        # Growing file: picked up on the next request; a half-written line waits.
        path = os.path.join(other, "wopo_hh.jsonl")
        r = rows(HH)[0]
        r["hand_id"] = "local-table:hand:999999"
        with open(path, "a") as f:
            f.write(json.dumps(r) + "\n" + '{"wopo_hh": 1, "hand_')
        st = os.stat(path)
        os.utime(path, ns=(st.st_atime_ns, st.st_mtime_ns + 1_000_000))
        lst = hist.hands()["hands"]
        self.assertEqual(len(lst), 15)
        self.assertIsNotNone(hist.hand("local-table:hand:999999"))

    def test_http_routes(self):
        hist = History([self.run])
        code, body, ctype = handle(hist, "/", "", live=False)
        self.assertEqual(code, 200)
        self.assertIn(b"<title>", body)
        self.assertIsNone(handle(hist, "/", "", live=True))  # the profile page owns / in live
        code, body, _ = handle(hist, "/api/history/hands", "", live=True)
        lst = json.loads(body)
        self.assertTrue(lst["live"])
        key = lst["hands"][0]["key"]
        code, body, _ = handle(hist, "/api/history/hand", "key=" + key, live=True)
        self.assertEqual(code, 200)
        self.assertEqual(json.loads(body)["key"], key)
        code, _, _ = handle(hist, "/api/history/hand", "key=nope", live=True)
        self.assertEqual(code, 404)
        self.assertIsNone(handle(hist, "/api/snapshot", "", live=True))


if __name__ == "__main__":
    unittest.main()

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

    def test_win_max_uses_server_winners(self):
        from opponent_analysis.history import summarize
        r = next(x for x in rows(HH) if x["hand_id"].endswith("000007"))
        h = build_hand(r)
        h["key"] = r["hand_id"]
        self.assertEqual(summarize(h)["win_max"], 50)  # seat 3, the uncalled 37 back
        # A stack reset inside the hand makes final - start meaningless; winners[] still holds.
        r2 = json.loads(json.dumps(r))
        r2["result"]["final_stacks"]["1"] += 10000
        h2 = build_hand(r2)
        h2["key"] = r2["hand_id"]
        self.assertEqual(summarize(h2)["win_max"], 50)
        r2["result"]["winners"] = []
        h3 = build_hand(r2)
        h3["key"] = r2["hand_id"]
        self.assertEqual(summarize(h3)["win_max"], max(x["net"] for x in h3["seats"]))

    def test_showdown_made_hands(self):
        from opponent_analysis.history import summarize
        r = json.loads(json.dumps(next(x for x in rows(HH) if x["hand_id"].endswith("000008"))))
        h = build_hand(r)
        sd = [x for x in h["seats"] if x["showdown"]]
        self.assertGreaterEqual(len(sd), 2)
        # Hand-set cards: the winner holds a flush, the other showdown seat a full house (only the
        # evaluation is tested, not who should have won); folded seats get no type.
        r["board"] = ["2h", "7h", "9h", "Kc", "Kd"]
        winner = r["result"]["winners"][0]["seat"]
        others = [x["seat"] for x in sd if x["seat"] != winner]
        shown = {str(winner): ["Ah", "3h"], **{str(s): ["9c", "9d"] for s in others[:1]}}
        if r["my_seat"] == winner:
            r["hole"] = ["Ah", "3h"]
            shown.pop(str(winner))
        elif r["my_seat"] in others[:1]:
            r["hole"] = ["9c", "9d"]
            shown.pop(str(r["my_seat"]))
        r["result"]["shown_cards"] = shown
        h = build_hand(r)
        made = {x["seat"]: x["made"] for x in h["seats"]}
        self.assertEqual(made[winner], 5)  # flush
        self.assertEqual(made[others[0]], 6)  # nines full of kings
        self.assertTrue(all(made[x["seat"]] is None for x in h["seats"] if not x["showdown"]))
        h["key"] = r["hand_id"]
        self.assertEqual(summarize(h)["win_made"], [5])  # only the winner's type counts

    def test_no_showdown_no_made_hand(self):
        from opponent_analysis.history import summarize
        r = next(x for x in rows(HH) if x["hand_id"].endswith("000007"))  # everyone folds to a river bet
        h = build_hand(r)
        h["key"] = r["hand_id"]
        self.assertEqual(summarize(h)["win_made"], [])

    def test_failed_validation_is_a_warning_not_a_drop(self):
        r = json.loads(json.dumps(rows(HH)[0]))
        fs = r["result"]["final_stacks"]
        fs["0"] += 5  # not zero-sum, like wopo's extra all-in deductions
        h = build_hand(r)
        self.assertIsNotNone(h["check"])
        self.assertEqual(h["steps"][-1]["kind"], "result")


class RangeStrength(unittest.TestCase):
    # Header + three advisor range-log lines (flop / turn / river) from the 2026-11-01 run, which
    # carry the advisor's own `strength` next to the weights.
    SAMPLE = os.path.join(DATA, "wopo_ranges_2026_11_01_sample.jsonl")

    def test_hand_category_matches_best_rank(self):
        import random
        from opponent_analysis.cards import DECK, best_rank
        from opponent_analysis.history import hand_category
        rnd = random.Random(7)
        hands = [rnd.sample(DECK, n) for n in (5, 6, 7) for _ in range(3000)]
        hands += [["Ah", "Kh", "Qh", "Jh", "Th", "2c", "3d"], ["5s", "4s", "3s", "2s", "As", "Kd", "Kc"],
                  ["Ad", "2c", "3h", "4s", "5d"], ["9c", "8c", "7c", "6c", "5c", "4c", "Ac"]]
        for cs in hands:
            br = best_rank(cs)
            want = 9 if br[:2] == (8, 12) else br[0]  # src/eval.rs: A-high straight flush = RoyalFlush
            self.assertEqual(hand_category(cs)[0], want, cs)

    def test_backfill_reproduces_the_advisor(self):
        from opponent_analysis.history import backfill_strength
        lines = rows(self.SAMPLE)
        combos = lines[0]["combos"]
        n = 0
        for rec in lines[1:]:
            want = {s["seat"]: s["strength"] for s in rec["seats"] if "strength" in s}
            self.assertTrue(want)
            bare = json.loads(json.dumps(rec))
            for s in bare["seats"]:
                s.pop("strength", None)
            self.assertTrue(backfill_strength(bare, combos))
            got = {s["seat"]: s["strength"] for s in bare["seats"] if "strength" in s}
            self.assertEqual(set(got), set(want))  # opponents only, never the hero seat
            for seat, w in want.items():
                n += 1
                self.assertEqual(set(got[seat]), set(w))
                for k in w:  # the log's weights are the advisor's x1e6 rounded
                    self.assertAlmostEqual(got[seat][k], w[k], delta=2e-4)
            # A line that already has it is left alone.
            self.assertFalse(backfill_strength(rec, combos))
        self.assertGreater(n, 0)

    def test_load_run_backfills_and_marks(self):
        with tempfile.TemporaryDirectory() as d:
            shutil.copy(HH, os.path.join(d, "wopo_hh.jsonl"))
            lines = rows(self.SAMPLE)
            hh = rows(HH)[1]  # attach the sample lines to a hand of the hh fixture
            with open(os.path.join(d, "wopo_ranges.jsonl"), "w") as f:
                f.write(json.dumps(lines[0]) + "\n")
                for rec in lines[1:]:
                    rec = json.loads(json.dumps(rec))
                    rec["hand_id"] = hh["hand_id"]
                    for s in rec["seats"]:
                        s.pop("strength", None)
                    f.write(json.dumps(rec) + "\n")
            run = load_run(os.path.join(d, "wopo_hh.jsonl"))
            h = next(x for x in run["hands"] if x["hand_id"] == hh["hand_id"])
            self.assertEqual(sorted(h["ranges"]), sorted(r["street"] for r in lines[1:]))
            self.assertTrue(all(r["backfill"] for r in h["ranges"].values()))
            self.assertEqual(run["counts"]["ranges_backfilled"], 3)
            from opponent_analysis.history import summarize
            h["key"] = h["hand_id"]
            self.assertTrue(summarize(h)["has_ranges"])


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
        # Run list: newest hh file first.
        a, b = os.path.join(self.run, "wopo_hh.jsonl"), os.path.join(other, "wopo_hh.jsonl")
        os.utime(a, (1_800_000_000, 1_800_000_000))
        os.utime(b, (1_700_000_000, 1_700_000_000))
        self.assertEqual([r["run"] for r in hist.hands()["runs"]], ["wopo_run_a", "wopo_run_b"])
        os.utime(b, (1_900_000_000, 1_900_000_000))
        self.assertEqual([r["run"] for r in hist.hands()["runs"]], ["wopo_run_b", "wopo_run_a"])
        lst = hist.hands()["hands"]
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

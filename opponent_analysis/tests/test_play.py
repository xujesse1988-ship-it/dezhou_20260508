"""Action guide: following the wopo driver's --play-events stream and serving /api/play."""

import gzip
import json
import os
import shutil
import tempfile
import threading
import types
import unittest
import urllib.error
import urllib.request
from http.server import ThreadingHTTPServer

from opponent_analysis.live import (Follower, PlayFeed, accepts_gzip, default_range_patterns, hand_view, make_handler,
                                    range_event)

DATA = os.path.join(os.path.dirname(__file__), "data")
# tools/wopo_play.py --selftest writes and checks this file (ts / decide_ms normalised): the
# 2026-10-16 session replayed with a stub advisor.
EVENTS = os.path.join(DATA, "wopo_play_events_2026_10_16.jsonl")
HH = os.path.join(DATA, "wopo_hh_2026_10_16.jsonl")


def load(path):
    with open(path, encoding="utf-8") as f:
        return [json.loads(line) for line in f if line.strip()]


def by_hand(events):
    out = {}
    for e in events:
        out.setdefault(e["hand_id"], []).append(e)
    return out


class FollowerTailTest(unittest.TestCase):
    def test_tail_skips_partial_line_and_replaced_file_reads_from_start(self):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "ev.jsonl")
            lines = [json.dumps({"n": i}) + "\n" for i in range(100)]
            with open(p, "w") as f:
                f.writelines(lines)
            size = os.path.getsize(p)
            tail = sum(len(x) for x in lines[-5:]) + 3  # starts 3 bytes into line 94
            got = [r["n"] for _, _, r in Follower([p], tail_bytes=tail).poll()]
            self.assertEqual(got, [95, 96, 97, 98, 99])
            # Starting exactly at a line start keeps that line.
            got = [r["n"] for _, _, r in Follower([p], tail_bytes=tail - 3).poll()]
            self.assertEqual(got, [95, 96, 97, 98, 99])
            self.assertEqual(len(Follower([p], tail_bytes=size + 10).poll()), 100)
            fo = Follower([p], tail_bytes=tail)
            fo.poll()
            with open(p, "a") as f:
                f.write('{"n": 100}\n')
            self.assertEqual([r["n"] for _, _, r in fo.poll()], [100])
            os.replace(p, p + ".old")  # a new file at the same path is read from its start
            with open(p, "w") as f:
                f.writelines(lines[:3])
            self.assertEqual([r["n"] for _, _, r in fo.poll()], [0, 1, 2])


class SessionInfoTest(unittest.TestCase):
    """/api/play `session`: the driver session behind the events file (start, pid, hands so far)."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.run_dir = os.path.join(self.tmp.name, "run")
        os.makedirs(self.run_dir)
        self.path = os.path.join(self.run_dir, "wopo_play_events.jsonl")
        self.events = load(EVENTS)
        dead = dict(self.events[0], type="dead", text="本手作废")  # one voided hand on top of the fixture
        self.events.append(dict(dead, ts=self.events[-1]["ts"] + 1))

    def write(self, recs):
        with open(self.path, "a", encoding="utf-8") as f:
            f.writelines(json.dumps(r, ensure_ascii=False) + "\n" for r in recs)

    def pid_file(self, pid, mtime):
        p = os.path.join(self.run_dir, "driver.pid")
        with open(p, "w") as f:
            f.write(f"{pid}\n")
        os.utime(p, (mtime, mtime))

    def counts(self, events, start=None):
        n = lambda t: sum(e["type"] == t and (start is None or e["ts"] >= start) for e in events)
        return n("hand_start"), n("result"), n("dead")

    def session(self, feed):
        s = feed.response(None)["session"]
        return s, (s["hands"], s["settled"], s["dead"])

    def test_no_pid_file_counts_whole_file_from_first_event(self):
        feed = PlayFeed([self.path], max_hands=3)
        feed.step()
        self.assertIsNone(feed.response(None)["session"])  # no file yet
        half = len(self.events) // 2
        self.write(self.events[:half])
        feed.step()
        s, c = self.session(feed)
        self.assertEqual((s["started"], s["start_src"], s["pid"], s["alive"]),
                         (self.events[0]["ts"], "first_event", None, None))
        self.assertEqual(c, self.counts(self.events[:half]))
        # Appended events (and a half-written line) count once the line is complete.
        tail = json.dumps(self.events[half], ensure_ascii=False)
        with open(self.path, "a", encoding="utf-8") as f:
            f.write(tail[:10])
        feed.step()
        self.assertEqual(self.session(feed)[1], self.counts(self.events[:half]))
        with open(self.path, "a", encoding="utf-8") as f:
            f.write(tail[10:] + "\n")
        self.write(self.events[half + 1:])
        v = feed.response(None)["version"]
        feed.step()
        self.assertGreater(feed.response(None)["version"], v)
        self.assertEqual(self.session(feed)[1], self.counts(self.events))
        self.assertEqual(self.counts(self.events), (8, 7, 1))

    def test_mirror_alive_file_overrides_local_pid_check(self):
        """A mirrored run directory (scripts/wopo_mirror.sh): the pid is the remote driver's, so liveness comes
        from driver.alive; stale, for another pid or unreadable → unknown; removed → local check again."""
        import time
        self.write(self.events)
        start = self.events[0]["ts"]
        remote_pid = 999999  # no such local process
        self.pid_file(remote_pid, start)
        alive_path = os.path.join(self.run_dir, "driver.alive")

        def mirror(**rec):
            with open(alive_path, "w", encoding="utf-8") as f:
                json.dump(rec, f)

        feed = PlayFeed([self.path], max_hands=3)
        cases = [
            (dict(pid=remote_pid, alive=True, checked=time.time()), True),
            (dict(pid=remote_pid, alive=False, checked=time.time()), False),
            (dict(pid=remote_pid, alive=True, checked=time.time() - 120), None),  # mirror stopped syncing
            (dict(pid=remote_pid + 1, alive=True, checked=time.time()), None),  # pid file not synced yet
            (dict(pid=remote_pid, alive="yes", checked=time.time()), None),
        ]
        for rec, want in cases:
            mirror(**rec)
            feed.step()
            self.assertIs(self.session(feed)[0]["alive"], want, rec)
        with open(alive_path, "w") as f:
            f.write("{half")
        feed.step()
        self.assertIsNone(self.session(feed)[0]["alive"])
        os.remove(alive_path)
        feed.step()
        self.assertIs(self.session(feed)[0]["alive"], False)  # back to the local check: no such process

    def test_pid_file_sets_start_liveness_and_counts_only_later_hands(self):
        self.write(self.events)
        start = [e["ts"] for e in self.events if e["type"] == "hand_start"][3]  # a restart before hand 4
        import subprocess
        import sys
        # Stands in for tools/wopo_play.py: the marker is in its command line.
        driver = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)", "wopo_play"])
        self.addCleanup(driver.wait)
        self.addCleanup(driver.kill)
        self.pid_file(driver.pid, start)
        feed = PlayFeed([self.path], max_hands=3)
        feed.step()
        s, c = self.session(feed)
        self.assertEqual((s["started"], s["start_src"], s["pid"], s["alive"]), (start, "driver.pid", driver.pid, True))
        self.assertEqual(c, self.counts(self.events, start))
        self.assertEqual(c[0], 5)
        # A live pid that is not the driver (reused number) is not alive.
        self.pid_file(os.getpid(), start)
        feed.step()
        self.assertIs(self.session(feed)[0]["alive"], False)
        self.pid_file(driver.pid, start)
        feed.step()
        self.assertIs(self.session(feed)[0]["alive"], True)
        # The driver exits: published without any new event.
        gone = subprocess.Popen(["true"])
        gone.wait()
        self.pid_file(gone.pid, start)
        v = feed.response(None)["version"]
        self.assertEqual(feed.step(), 0)
        s, _ = self.session(feed)
        self.assertEqual((s["pid"], s["alive"]), (gone.pid, False))
        self.assertGreater(feed.response(None)["version"], v)
        # Restarted (pid file rewritten later): the count starts over from the new start.
        later = self.events[-1]["ts"] + 10
        self.pid_file(driver.pid, later)
        feed.step()
        self.assertEqual(self.session(feed)[1], (0, 0, 0))
        self.write([dict(e, ts=later + 1 + i) for i, e in enumerate(self.events[:3])])
        feed.step()
        self.assertEqual(self.session(feed)[1], self.counts(self.events[:3]))

    def test_hero_vpip_pfr_over_session_hands(self):
        def act(hid, ts, kind, delta=0, to_call=0, street="preflop"):
            return {"ts": ts, "type": "hero_action", "hand_id": hid, "street": street, "action": kind,
                    "delta": delta, "to_call_before": to_call}
        recs = [
            act("h0", 100, "raise", 300, 100),  # before the start: not counted
            act("h1", 200, "call", 100, 100),  # VPIP
            act("h2", 210, "raise", 450, 100), act("h2", 211, "fold", street="flop"),  # VPIP + PFR, counted once
            act("h3", 220, "check"), act("h3", 221, "raise", 300, 0, street="flop"),  # BB check: neither; flop raise ignored
            act("h4", 230, "fold", 0, 100),
            act("h5", 240, "all_in", 800, 800),  # all-in call: VPIP only
            act("h6", 250, "all_in", 900, 450),  # all-in raise: VPIP + PFR
            act("h7", 260, "call", 50, 50, street="flop"),  # no preflop action (walk / not dealt): not in the denominator
        ]
        self.write(recs)
        self.pid_file(os.getpid(), 150)
        feed = PlayFeed([self.path], max_hands=3)
        feed.step()
        s, _ = self.session(feed)
        self.assertEqual((s["hero_pre"], s["hero_vpip"], s["hero_pfr"]), (6, 4, 2))
        # Appended later: picked up incrementally; a restart starts the counts over.
        self.write([act("h8", 270, "raise", 300, 100)])
        feed.step()
        s, _ = self.session(feed)
        self.assertEqual((s["hero_pre"], s["hero_vpip"], s["hero_pfr"]), (7, 5, 3))
        self.pid_file(os.getpid(), 265)
        feed.step()
        s, _ = self.session(feed)
        self.assertEqual((s["hero_pre"], s["hero_vpip"], s["hero_pfr"]), (1, 1, 1))

    def test_newest_file_is_the_session(self):
        old = os.path.join(self.tmp.name, "old", "wopo_play_events.jsonl")
        os.makedirs(os.path.dirname(old))
        with open(old, "w", encoding="utf-8") as f:
            f.writelines(json.dumps(e, ensure_ascii=False) + "\n" for e in self.events)
        os.utime(old, (1, 1))
        self.write(self.events[:20])
        feed = PlayFeed([os.path.join(self.tmp.name, "*", "wopo_play_events.jsonl")], max_hands=3)
        feed.step()
        s, c = self.session(feed)
        self.assertEqual(s["path"], self.path)
        self.assertEqual(c, self.counts(self.events[:20]))


class PlayFeedTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.path = os.path.join(self.tmp.name, "run", "wopo_play_events.jsonl")
        self.events = load(EVENTS)

    def append(self, recs, tail=""):
        os.makedirs(os.path.dirname(self.path), exist_ok=True)
        with open(self.path, "a", encoding="utf-8") as f:
            for r in recs:
                f.write(json.dumps(r, ensure_ascii=False) + "\n")
            f.write(tail)

    def test_missing_file_then_incremental_append_and_since(self):
        feed = PlayFeed([self.path], max_hands=20)
        self.assertEqual(feed.step(), 0)  # the driver has not created the file yet
        r = feed.response(None)
        self.assertEqual((r["enabled"], r["hands"], r["files"]), (True, [], []))
        v0 = r["version"]
        r = feed.response(str(v0))
        self.assertEqual({k: x for k, x in r.items() if k != "now"}, {"enabled": True, "version": v0, "unchanged": True})
        # Event by event, the last line sometimes half written: the current hand always matches
        # the events written so far.
        ev = self.events
        i = 0
        while i < len(ev):
            j = min(len(ev), i + 7)
            half = json.dumps(ev[j], ensure_ascii=False)[:20] if j < len(ev) else ""
            self.append(ev[i:j], tail=half)
            v_before = feed.response(None)["version"]
            self.assertEqual(feed.step(), j - i)
            r = feed.response(str(v_before))
            self.assertNotIn("unchanged", r)
            self.assertEqual(r["version"], v_before + 1)
            cur = r["hands"][0]
            self.assertEqual(cur["hand_id"], ev[j - 1]["hand_id"])
            self.assertEqual(cur["events"], [e for e in ev[:j] if e["hand_id"] == cur["hand_id"]])
            self.assertTrue(feed.response(str(r["version"])).get("unchanged"))
            if half:  # drop the half line again: the next chunk writes it whole
                with open(self.path, "rb+") as f:
                    f.truncate(os.path.getsize(self.path) - len(half.encode()))
            i = j
        self.assertEqual(feed.step(), 0)
        r = feed.response(None)
        self.assertEqual([h["hand_id"] for h in r["hands"]], list(reversed(list(by_hand(ev)))))
        self.assertEqual(r["counts"], {"events": len(ev)})

    def test_unreadable_hand_keeps_the_view_shape(self):
        # One hand's seats event is garbage: that hand becomes status "error" with every field the
        # page reads (turns, seats, ...) still present, and the other hands are exactly as before.
        hands = by_hand(self.events)
        bad_id = list(hands)[1]
        events = [dict(e, seats="garbage") if e["hand_id"] == bad_id and e["type"] == "seats" else e
                  for e in self.events]
        self.append(events)
        feed = PlayFeed([self.path], max_hands=20)
        feed.step()
        r = feed.response(None)
        clean = {h["hand_id"]: h for h in (hand_view(k, v) for k, v in hands.items())}
        got = {h["hand_id"]: h for h in r["hands"]}
        bad = got.pop(bad_id)
        self.assertEqual(bad["status"], "error")
        self.assertTrue(bad["error"])
        self.assertLessEqual(set(clean[bad_id]), set(bad))
        self.assertEqual((bad["turns"], bad["seats"], bad["partial"]), ([], [], False))
        self.assertEqual((bad["first_ts"], bad["last_ts"], bad["table_id"]),
                         (clean[bad_id]["first_ts"], clean[bad_id]["last_ts"], clean[bad_id]["table_id"]))
        self.assertEqual(got, {k: v for k, v in clean.items() if k != bad_id})
        self.assertTrue(any(str(bad_id) in e["msg"] for e in r["errors"]))

    def test_restart_recovers_last_hands_from_tail(self):
        self.append(self.events)
        full = PlayFeed([self.path], max_hands=50)
        full.step()
        want = full.response(None)["hands"]
        # Small tail: starts mid-file (mid-line, mid-hand) and still recovers the newest hands.
        feed = PlayFeed([self.path], max_hands=3, tail_bytes=os.path.getsize(self.path) // 3)
        feed.step()
        got = feed.response(None)["hands"]
        self.assertEqual(got, want[:3])
        self.assertTrue(all(not h["partial"] for h in got))
        # A copy of the same file replacing it: everything is re-read, nothing changes.
        v = feed.response(None)["version"]
        shutil.copy(self.path, self.path + ".tmp")
        os.replace(self.path + ".tmp", self.path)
        self.assertEqual(feed.step(), 0)
        self.assertEqual(feed.response(None)["version"], v)
        self.assertGreater(feed.counts["late_event"] + feed.counts["duplicate"], 0)

    def test_window_eviction_and_late_events(self):
        self.append(self.events)
        feed = PlayFeed([self.path], max_hands=2)
        feed.step()
        hands = list(by_hand(self.events))
        self.assertEqual([h["hand_id"] for h in feed.response(None)["hands"]], hands[:-3:-1])
        # A stale decision for a hand that already left the window does not bring it back.
        late = dict(next(e for e in self.events if e["type"] == "decision"), ts=1792139999.0)
        self.append([late])
        self.assertEqual(feed.step(), 0)
        self.assertEqual(feed.counts["late_event"], 1)

    def test_range_log_strength_per_street(self):
        """The advisor's --range-log next to the events adds each opponent's strength mix per street
        (latest solve of the street wins); hero seats, the header and the 1326 weights are dropped."""
        self.append(self.events)
        hid = self.events[-1]["hand_id"]
        t0 = self.events[-1]["ts"]
        rpath = default_range_patterns([self.path])[0]
        self.assertEqual(rpath, os.path.join(os.path.dirname(self.path), "wopo_ranges.jsonl"))

        def line(street, ts, strength, board):
            return {"ts": ts, "hand_id": hid, "trigger": "prewarm", "source": "prewarm:stored", "street": street,
                    "board": board, "path": "unanchored", "range_src": "prefix_reach",
                    "seats": [{"seat": 0, "name": "hero", "hero": True, "range": [1] * 1326},
                              {"seat": 3, "name": "v", "hero": False, "coef": True, "width": False,
                               "range": [1] * 1326, "strength": strength}]}
        recs = [{"range_log_header": 1, "combos": []},
                line("flop", t0 + 1, {"nothing": 0.6, "pair": 0.4}, ["As", "7d", "2c"]),
                line("flop", t0 + 2, {"nothing": 0.5, "top_pair": 0.5}, ["As", "7d", "2c"]),
                line("turn", t0 + 3, {"trips": 0.2, "nothing": 0.8}, ["As", "7d", "2c", "2h"]),
                {"ts": t0, "hand_id": None, "street": "flop", "seats": []}]
        with open(rpath, "w", encoding="utf-8") as f:
            f.writelines(json.dumps(r) + "\n" for r in recs)
        self.assertIsNone(range_event(recs[0]))
        self.assertIsNone(range_event(recs[-1]))
        ev = range_event(recs[1])
        self.assertEqual(ev["seats"], [{"seat": 3, "name": "v", "coef": True, "width": False,
                                        "strength": {"nothing": 0.6, "pair": 0.4}}])
        feed = PlayFeed([self.path], max_hands=3, range_patterns=[rpath])
        feed.step()
        r = feed.response(None)
        cur = r["hands"][0]
        self.assertEqual(cur["hand_id"], hid)
        self.assertEqual([x["street"] for x in cur["ranges"]], ["flop", "turn"])
        self.assertEqual(cur["ranges"][0]["seats"][0]["strength"], {"nothing": 0.5, "top_pair": 0.5})
        self.assertEqual(cur["ranges"][1]["board"], ["As", "7d", "2c", "2h"])
        self.assertTrue(all("range" not in s for e in cur["events"] for s in e.get("seats") or []))
        self.assertEqual([f["lines"] for f in r["range_files"]], [5])
        # Without range files the views have no strength and are otherwise unchanged.
        plain = PlayFeed([self.path], max_hands=3)
        plain.step()
        p = plain.response(None)["hands"]
        self.assertEqual(p[0]["ranges"], [])
        self.assertEqual(p[1:], r["hands"][1:])

    def test_views_match_hand_history(self):
        """The guide's per-hand state equals what the driver's hh log says about the same hands."""
        views = {h: hand_view(h, evs) for h, evs in by_hand(self.events).items()}
        hh = load(HH)
        self.assertEqual(len(hh), 7)
        for r in hh:
            v = views[r["hand_id"]]
            self.assertEqual((v["status"], v["partial"], v["dead"]), ("done", False, None))
            self.assertEqual((v["button"], v["sb"], v["bb"], v["hero_seat"]),
                             (r["button_seat"], r["small_blind"], r["big_blind"], r["my_seat"]))
            self.assertEqual(v["hole"], r["hole"])
            self.assertEqual(v["board"], r["board"])
            self.assertEqual({x["seat"]: x["start"] for x in v["seats"]},
                             {int(s): n for s, n in r["initial_stacks"].items()})
            self.assertEqual({x["seat"]: x["name"] for x in v["seats"]},
                             {int(s): n for s, n in r["names"].items()})
            acts = [(e["seat"], e["action"], e["to"], e["delta"], e["street"]) for e in v["events"]
                    if e["type"] in ("action", "hero_action")]
            self.assertEqual(acts, [(a["seat"], a["action"], a.get("to"), a["contribution_delta"], a["street"])
                                    for a in r["actions"]])
            final = {int(s): n for s, n in r["result"]["final_stacks"].items()}
            ring = r["dealt_seats"][r["dealt_seats"].index(r["button_seat"]) + 1:] + r["dealt_seats"]
            blind = {ring[0]: r["small_blind"], ring[1]: r["big_blind"]}
            put = {s: blind.get(s, 0) + sum(a["contribution_delta"] for a in r["actions"] if a["seat"] == s)
                   for s in r["dealt_seats"]}
            # Seat stacks follow every action (before the pot is paid out); the result has the finals.
            self.assertEqual({x["seat"]: x["stack"] for x in v["seats"]},
                             {s: r["initial_stacks"][str(s)] - put[s] for s in r["dealt_seats"]})
            self.assertEqual({x["seat"]: x["final"] for x in v["result"]["stacks"]},
                             {s: final[s] for s in r["dealt_seats"]})
            self.assertEqual(v["pot"], sum(a["contribution_delta"] for a in r["actions"])
                             + r["small_blind"] + r["big_blind"])
            self.assertEqual(v["hero_net"], final[0] - r["initial_stacks"]["0"])
            folded = {a["seat"] for a in r["actions"] if a["action"] == "fold"}
            self.assertEqual({x["seat"] for x in v["seats"] if x["folded"]}, folded)
            # Every hero decision point has its decision and hero's actual action, in order.
            hero_acts = [a for a in r["actions"] if a["seat"] == 0]
            self.assertEqual(len(v["turns"]), len(hero_acts))
            for t, a in zip(v["turns"], hero_acts):
                self.assertEqual(t["hero_action"]["action"], a["action"])
                d = t["decision"]
                self.assertTrue(d["written"])
                self.assertIsNone(d["stale"])
                self.assertEqual((d["source"], d["probs"]), ("stub", [[d["action"], 1.0]]))
                same = d["action"] == a["action"] or {d["action"], a["action"]} <= {"raise", "all_in"}
                self.assertEqual(t["hero_action"]["match"], "same" if same else "diff")
                self.assertEqual(t["hero_action"]["advice"]["text"], d["advice"])
        # The 8th hand is cut off mid-hand in the recording: still playing.
        last = views[list(views)[-1]]
        self.assertEqual((last["status"], last["result"]), ("playing", None))

    def test_dead_stale_and_unadvised(self):
        base = by_hand(self.events)["local-table:hand:000008"]
        start = [e for e in base if e["type"] in ("hand_start", "hole", "seats")]
        turn_ev = next(e for e in base if e["type"] == "hero_turn")
        k = turn_ev["turn"]
        # hero acts before the search returns; the decision arrives stale after the hand died.
        hero = dict(next(e for e in base if e["type"] == "hero_action"), advice=None, match=None)
        dead = {"ts": hero["ts"] + 1, "type": "dead", "hand_id": base[0]["hand_id"], "table_id": "local-table",
                "reason": "disconnected", "tag": "disconnected", "text": "本手作废：disconnected（之后不出建议）"}
        dec = dict(next(e for e in base if e["type"] == "decision" and e["turn"] == k), ts=hero["ts"] + 2,
                   stale="hand_dead:disconnected", stale_kind="hand_dead", written=False, decide_ms=5216)
        v = hand_view(base[0]["hand_id"], start + [turn_ev, hero, dead, dec])
        self.assertEqual(v["status"], "dead")
        self.assertEqual(v["dead"]["reason"], "disconnected")
        (t,) = v["turns"]
        self.assertEqual((t["turn"], t["hero_action"]["match"], t["hero_action"]["advice"]), (k, None, None))
        self.assertEqual((t["decision"]["stale_kind"], t["decision"]["written"], t["decision"]["decide_ms"]),
                         ("hand_dead", False, 5216))
        # A dead hand that never started (bad hand_start) still shows up, by its hand_id.
        v = hand_view("x:hand:1", [dict(dead, hand_id="x:hand:1")])
        self.assertEqual((v["status"], v["partial"], v["seats"]), ("dead", True, []))


class PlayHttpTest(unittest.TestCase):
    def serve(self, play):
        live = types.SimpleNamespace(lock=threading.Lock(), snapshot={}, md="", profile={})
        httpd = ThreadingHTTPServer(("127.0.0.1", 0), make_handler(live, play))
        threading.Thread(target=httpd.serve_forever, daemon=True).start()
        self.addCleanup(httpd.server_close)
        self.addCleanup(httpd.shutdown)
        return f"http://127.0.0.1:{httpd.server_address[1]}"

    def get(self, url):
        with urllib.request.urlopen(url, timeout=5) as r:
            return r.status, r.headers.get("Content-Type"), r.read()

    def test_api_play(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "ev.jsonl")
            shutil.copy(EVENTS, path)
            feed = PlayFeed([path])
            feed.step()
            base = self.serve(feed)
            code, ctype, body = self.get(base + "/api/play")
            r = json.loads(body)
            self.assertEqual((code, ctype), (200, "application/json; charset=utf-8"))
            self.assertEqual((r["enabled"], len(r["hands"])), (True, 8))
            r2 = json.loads(self.get(f"{base}/api/play?since={r['version']}")[2])
            self.assertEqual((r2["version"], r2["unchanged"]), (r["version"], True))
            self.assertIn("now", r2)
            self.assertNotIn("unchanged", json.loads(self.get(f"{base}/api/play?since=0")[2]))
            code, ctype, page = self.get(base + "/")
            self.assertEqual(code, 200)
            self.assertIn("行动指南".encode(), page)

    def test_gzip_when_accepted(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "ev.jsonl")
            shutil.copy(EVENTS, path)
            feed = PlayFeed([path])
            feed.step()
            base = self.serve(feed)
            plain = self.get(base + "/api/play")[2]

            def fetch(url, enc):
                req = urllib.request.Request(url, headers={"Accept-Encoding": enc})
                with urllib.request.urlopen(req) as r:
                    return r.headers.get("Content-Encoding"), r.headers.get("Vary"), r.read()

            # Large body + gzip accepted: compressed, same JSON once decompressed (the "now" field
            # is the server clock, so compare without it).
            enc, vary, body = fetch(base + "/api/play", "gzip, deflate")
            self.assertEqual((enc, vary), ("gzip", "Accept-Encoding"))
            self.assertLess(len(body), len(plain) / 3)
            strip = lambda b: {k: v for k, v in json.loads(b).items() if k != "now"}  # noqa: E731
            self.assertEqual(strip(gzip.decompress(body)), strip(plain))
            # Refused, or not offered: plain.
            self.assertIsNone(fetch(base + "/api/play", "gzip;q=0, identity")[0])
            self.assertIsNone(fetch(base + "/api/play", "identity")[0])
            # Small body (the "unchanged" reply): not worth compressing.
            v = json.loads(plain)["version"]
            self.assertIsNone(fetch(f"{base}/api/play?since={v}", "gzip")[0])

    def test_accepts_gzip(self):
        for header, want in [(None, False), ("", False), ("gzip", True), ("deflate, gzip;q=0.5", True),
                             ("gzip;q=0", False), ("GZIP", True), ("*", True), ("br, deflate", False)]:
            self.assertEqual(accepts_gzip(header), want, header)

    def test_api_play_disabled(self):
        base = self.serve(None)
        self.assertEqual(json.loads(self.get(base + "/api/play")[2]), {"enabled": False})

    def test_preflop_backend_mounted(self):
        class Backend:
            def handle(self, path, query):
                return (200, f"viewer {path} {query}".encode(), "text/plain") if path.startswith("/preflop/") else None

        live = types.SimpleNamespace(lock=threading.Lock(), snapshot={}, md="", profile={})
        httpd = ThreadingHTTPServer(("127.0.0.1", 0), make_handler(live, None, None, Backend()))
        threading.Thread(target=httpd.serve_forever, daemon=True).start()
        self.addCleanup(httpd.server_close)
        self.addCleanup(httpd.shutdown)
        base = f"http://127.0.0.1:{httpd.server_address[1]}"
        self.assertEqual(self.get(base + "/preflop/api/meta?x=1")[2], b"viewer /preflop/api/meta x=1")
        self.assertEqual(self.get(base + "/preflop")[2], b"viewer /preflop/ ")  # redirected to the slash
        with self.assertRaises(urllib.error.HTTPError) as e:  # anything else still falls through to 404
            self.get(base + "/nope")
        self.assertEqual(e.exception.code, 404)

    def test_no_preflop_backend_is_404(self):
        base = self.serve(None)
        with self.assertRaises(urllib.error.HTTPError) as e:
            self.get(base + "/preflop/")
        self.assertEqual(e.exception.code, 404)


if __name__ == "__main__":
    unittest.main()

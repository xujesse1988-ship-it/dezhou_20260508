"""Action guide: following the wopo driver's --play-events stream and serving /api/play."""

import gzip
import json
import os
import shutil
import tempfile
import threading
import types
import unittest
import urllib.request
from http.server import ThreadingHTTPServer

from opponent_analysis.live import Follower, PlayFeed, accepts_gzip, hand_view, make_handler

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


if __name__ == "__main__":
    unittest.main()

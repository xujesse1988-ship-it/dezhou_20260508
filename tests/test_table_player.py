"""Regression tests for local WS playback; no external service or API key needed."""

from copy import deepcopy
from http.client import HTTPConnection
from http.server import ThreadingHTTPServer
import json
from pathlib import Path
import tempfile
import threading
import unittest

from table_player.server import Sources, handler_for
from table_player.state import LogStore, TableState, normalize


def snapshot(hand="h1", seq=2, hero=3, n=6, **extra):
    data = dict(type="table_state", table_id="table-a", hand_id=hand, table_seq=seq,
                street="flop", board=["Ah", "7s", "2c"], pot=200, actor_seat=hero, dealer_seat=1,
                seats=[dict(seat=i, name=f"p{i}", stack=1800, bet=0, status="active",
                            in_hand=True, folded=False) for i in range(n)],
                hero={"seat": hero, "hole_cards": ["As", "Kh"]})
    data.update(extra)
    return data


def line(message, **extra):
    return json.dumps(dict(dir="in", ts=100, raw=json.dumps(message), **extra)) + "\n"


class TableStateTests(unittest.TestCase):
    def test_snapshot_infers_size_and_hero_without_revealing_opponents(self):
        model = TableState()
        model.apply(snapshot())
        self.assertEqual(model.data["num_seats"], 6)
        self.assertEqual(model.data["hero_seat"], 3)
        self.assertEqual(model.data["hole"], ["As", "Kh"])
        self.assertTrue(all(not p["cards"] for p in model.data["players"]))

    def test_new_hand_and_table_change_clear_private_cards(self):
        model = TableState()
        model.apply(snapshot())
        model.apply(dict(type="hand_start", table_id="table-a", hand_id="h2", seat=5,
                         table_seq=3, dealer_seat=2))
        self.assertEqual(model.data["hole"], [])
        self.assertEqual(model.data["board"], [])
        self.assertEqual(model.data["hero_seat"], 5)
        model.apply(dict(type="hole_cards", hand_id="h1", cards=["Qs", "Qh"]))
        self.assertEqual(model.data["hole"], [])
        model.apply(dict(type="table_joined", table_id="table-b", seat=1, table_seq=1, players=[]))
        self.assertEqual(model.data["hero_seat"], 1)
        self.assertIsNone(model.data["hand_id"])
        model.apply(dict(type="player_left", table_id="table-a", seat=1, table_seq=999))
        self.assertEqual(model.data["table_id"], "table-b")
        self.assertEqual(model.data["hero_seat"], 1)

    def test_same_hand_reseat_does_not_keep_previous_hero_cards(self):
        model = TableState()
        model.apply(snapshot())
        moved = snapshot(seq=3, hero=5)
        moved["hero"] = {"seat": 5}
        model.apply(moved)
        self.assertEqual(model.data["hole"], [])

    def test_snapshot_ahead_of_event_does_not_double_count_chips(self):
        model = TableState()
        initial = snapshot(seq=10)
        initial["seats"][2].update(stack=1700, bet=100)
        model.apply(initial)
        action = dict(type="player_action", table_id="table-a", hand_id="h1", table_seq=9,
                      seat=2, action="bet", amount=100, contribution_delta=100, stack_after=1700, pot=200)
        model.apply(action)
        self.assertEqual(model.data["players"][2]["bet"], 100)
        self.assertEqual(sum(h["kind"] == "action" for h in model.data["history"]), 1)
        model.apply(action)
        self.assertEqual(sum(h["kind"] == "action" for h in model.data["history"]), 1)
        model.apply(snapshot(seq=8, pot=50))
        self.assertEqual(model.data["pot"], 200)

    def test_closing_action_uses_current_street_and_contribution_delta(self):
        model = TableState()
        model.apply(snapshot())
        model.apply(dict(type="player_action", table_seq=3, seat=3, action="call", amount=None,
                         contribution_delta=30, stack_after=1770, street="turn", pot_after=230))
        self.assertEqual(model.data["street"], "flop")
        self.assertEqual(model.data["players"][3]["bet"], 30)
        model.apply(dict(type="community_cards", table_seq=4, street="turn", cards=["4d"]))
        self.assertEqual(model.data["board"], ["Ah", "7s", "2c", "4d"])
        self.assertEqual(model.data["players"][3]["bet"], 0)
        model.apply(dict(type="community_cards", table_seq=5, street="river", cards=["Ah", "7s", "2c", "4d", "9h"]))
        self.assertEqual(len(model.data["board"]), 5)

    def test_result_restores_board_after_idle_snapshot_and_uses_total_pot(self):
        model = TableState()
        original = snapshot()
        original["seats"][1]["folded"] = True
        model.apply(original)
        idle = snapshot(seq=3, street="idle", board=[], pot=0)
        for p in idle["seats"]:
            p.update(in_hand=False, folded=False)
        model.apply(idle)
        model.apply(dict(type="hand_result", table_seq=4, hand_id="h1", total_pot=200, pot=100,
                         final_stacks={"3": 2000}, winners=[{"seat": 3, "amount": 100}],
                         shown_cards={"2": ["Ad", "Qh"]}))
        self.assertEqual(model.data["board"], ["Ah", "7s", "2c"])
        self.assertTrue(model.data["players"][1]["folded"])
        self.assertEqual(model.data["pot"], 200)
        self.assertEqual(model.data["players"][2]["cards"], ["Ad", "Qh"])
        self.assertEqual(model.data["players"][3]["stack"], 2000)

    def test_resync_recovers_hero_and_rejects_old_snapshot(self):
        model = TableState()
        response = normalize({"type": "resync_response", "replayed_events": [], "snapshot": snapshot()})
        model.apply(response)
        self.assertEqual(model.data["hole"], ["As", "Kh"])
        before = deepcopy(model.data)
        model.apply(snapshot(seq=1, hero=0))
        self.assertEqual(model.data, before)

    def test_activity_keeps_all_events_in_latest_five_hands(self):
        model = TableState()
        for hand in range(1, 8):
            model.apply(snapshot(hand=f"h{hand}", seq=hand * 100))
            for action in range(45):
                model.apply(dict(type="player_action", hand_id=f"h{hand}", seat=3,
                                 table_seq=hand * 100 + action + 1, action="check"))
        history = model.data["history"]
        self.assertEqual({h["hand_id"] for h in history}, {"h3", "h4", "h5", "h6", "h7"})
        for hand in range(3, 8):
            entries = [h for h in history if h["hand_id"] == f"h{hand}"]
            self.assertEqual(sum(h["kind"] == "action" for h in entries), 45)
            self.assertEqual(next(h["cards"] for h in entries if h["kind"] == "hole"), ["As", "Kh"])
            self.assertEqual(entries[0]["hand_number"], hand)

    def test_activity_preserves_identity_and_units_across_tables(self):
        model = TableState()
        model.apply(snapshot(big_blind=20))
        model.apply(dict(type="player_action", seat=3, action="bet", amount=100, table_seq=3))
        old_history = deepcopy(model.data["history"])
        model.apply(snapshot(hand="h2", table_id="table-b", hero=1, big_blind=100))
        self.assertEqual(model.data["history"][:len(old_history)], old_history)
        action = next(h for h in model.data["history"] if h["kind"] == "action")
        self.assertEqual((action["name"], action["hero"], action["big_blind"]), ("p3", True, 20))
        self.assertEqual((action["table_id"], action["hand_id"]), ("table-a", "h1"))
        self.assertEqual(model.data["hand_number"], 2)
        self.assertEqual(model.data["hero_seat"], 1)

    def test_activity_records_cards_once_per_street_and_only_observed_reveals(self):
        model = TableState()
        model.apply(snapshot())
        model.apply(dict(type="hole_cards", hand_id="h1", cards=["As", "Kh"]))
        model.apply(snapshot(seq=3))
        model.apply(dict(type="community_cards", table_seq=4, street="flop", cards=["Ah", "7s", "2c"]))
        model.apply(dict(type="community_cards", table_seq=5, street="turn", cards=["4d"]))
        model.apply(snapshot(seq=6, street="turn", board=["Ah", "7s", "2c", "4d"]))
        model.apply(dict(type="your_turn", table_seq=7, seat=3,
                         community_cards=["Ah", "7s", "2c", "4d", "9h"]))
        model.apply(dict(type="community_cards", table_seq=8, street="river", cards=["9h"]))
        history = model.data["history"]
        self.assertEqual(sum(h["kind"] == "hole" for h in history), 1)
        boards = [h for h in history if h["kind"] == "board"]
        self.assertEqual([(h["street"], len(h["cards"])) for h in boards], [("flop", 3), ("turn", 4), ("river", 5)])
        self.assertEqual(boards[-1]["cards"], ["Ah", "7s", "2c", "4d", "9h"])
        self.assertFalse(any(h["kind"] == "shown" for h in history))
        model.apply(dict(type="hand_result", hand_id="h1", table_seq=9,
                         shown_cards={"2": ["Ad", "Qh"], "3": ["As", "Kh"]}))
        model.apply(dict(type="hand_start", hand_id="h2", table_seq=10, seat=3))
        shown = [h for h in model.data["history"] if h["kind"] == "shown"]
        self.assertEqual([(h["name"], h["cards"], h["hero"]) for h in shown],
                         [("p2", ["Ad", "Qh"], False), ("p3", ["As", "Kh"], True)])
        self.assertTrue(all(h["hand_id"] == "h1" and h["street"] == "showdown" for h in shown))
        self.assertEqual(model.data["board"], [])
        self.assertEqual(model.data["hole"], [])

    def test_settlement_keeps_each_winner_amount_pot_and_final_stacks(self):
        model = TableState()
        model.apply(snapshot(big_blind=20))
        result = dict(type="hand_result", hand_id="h1", table_seq=3, pot=70, total_pot=200,
                      winners=[{"seat": 2, "name": "Winner A", "amount": 50, "stack": 9999,
                                "hand_description": "Two pair"},
                               {"seat": 3, "amount": 20, "stack": 2020, "hand_description": None}],
                      final_stacks={"0": 0, "2": 2050, "3": 2020})
        model.apply(result)
        entry = next(h for h in model.data["history"] if h["kind"] == "result")
        saved = deepcopy(entry)
        self.assertEqual(entry["settlement"]["pot"], 200)
        self.assertEqual([(w["name"], w["amount"], w["hero"], w["description"])
                          for w in entry["settlement"]["winners"]],
                         [("Winner A", 50, False, "Two pair"), ("p3", 20, True, None)])
        self.assertEqual([(p["seat"], p["name"], p["stack"], p["hero"])
                          for p in entry["settlement"]["final_stacks"]],
                         [(0, "p0", 0, False), (2, "Winner A", 2050, False), (3, "p3", 2020, True)])
        self.assertEqual(model.data["players"][2]["stack"], 2050)
        model.apply(result)
        self.assertEqual(sum(h["kind"] == "result" for h in model.data["history"]), 1)
        model.data["result"]["winners"][0]["amount"] = 999
        model.apply(snapshot(hand="h2", table_id="table-b", hero=0, big_blind=100))
        self.assertEqual(next(h for h in model.data["history"] if h["kind"] == "result"), saved)

    def test_settlement_missing_fields_do_not_invent_chips_or_hand_types(self):
        model = TableState()
        model.apply(snapshot())
        model.apply(dict(type="hand_result", hand_id="h1", table_seq=3, total_pot=None, pot=30,
                         winners=[{"seat": 3, "stack": 1900}]))
        result = model.data["history"][-1]["settlement"]
        self.assertEqual(result["pot"], 30)
        self.assertIsNone(result["winners"][0]["amount"])
        self.assertIsNone(result["winners"][0]["description"])
        self.assertEqual(result["final_stacks"], [{"seat": 3, "name": "p3", "stack": 1900, "hero": True}])
        model.apply(dict(type="hand_start", hand_id="h2", table_seq=4))
        model.apply(dict(type="hand_result", hand_id="h2", table_seq=5, total_pot=0, winners=[]))
        result = model.data["history"][-1]["settlement"]
        self.assertEqual(result, {"pot": 0, "winners": [], "final_stacks": []})

    def test_outbound_and_secrets_are_not_rendered(self):
        self.assertIsNone(normalize({"dir": "out", "raw": '{"type":"action","turn_token":"secret"}'}))
        snap = snapshot()
        snap["hero"]["turn_token"] = "private-token"
        snap["authorization"] = "secret-key"
        clean = normalize(snap)
        self.assertNotIn("private-token", json.dumps(clean))
        self.assertNotIn("secret-key", json.dumps(clean))
        error = normalize({"dir": "event", "event": "error", "error": "Handshake status 403 Forbidden <html>secret-key</html>"})
        self.assertEqual(error["message"], "WebSocket HTTP 403")
        self.assertNotIn("secret-key", json.dumps(error))


class LogStoreTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name) / "openpoker_ws.jsonl"

    def test_partial_line_append_and_bad_line_do_not_break_live_tail(self):
        first = line(snapshot())
        self.path.write_text(first[:-8])
        store = LogStore(self.path)
        store.refresh()
        self.assertEqual(len(store.events), 0)
        self.assertTrue(store.view()["meta"]["partial_line"])
        with self.path.open("a") as f:
            f.write(first[-8:] + "not-json\n" + line(dict(type="hole_cards", hand_id="h1", cards=["Qs", "Qh"])))
        store.refresh()
        self.assertEqual(len(store.events), 2)
        self.assertEqual(store.invalid, 1)
        self.assertEqual(store.view()["state"]["hole"], ["Qs", "Qh"])

    def test_truncate_and_replace_reset_replay_generation(self):
        self.path.write_text(line(snapshot()))
        store = LogStore(self.path); store.refresh()
        old_generation = store.generation
        self.path.write_text(line(dict(type="connected", name="new")))
        store.refresh()
        self.assertGreater(store.generation, old_generation)
        self.assertIsNone(store.view()["state"]["hand_id"])
        replacement = self.path.with_suffix(".new")
        replacement.write_text(line(snapshot(hand="h9")))
        replacement.replace(self.path)
        store.refresh()
        self.assertEqual(store.view()["state"]["hand_id"], "h9")
        self.assertEqual(len(store.events), 1)

    def test_missing_file_can_appear_later(self):
        store = LogStore(self.path); store.refresh()
        self.assertTrue(store.missing)
        self.path.write_text(line(snapshot()))
        store.refresh()
        self.assertFalse(store.missing)
        self.assertEqual(store.view()["state"]["hero_seat"], 3)

    def test_checkpoint_seek_has_no_future_showdown_or_hole_cards(self):
        self.path.write_text(line(snapshot()) + ''.join(line(dict(type="player_action", seat=2,
            action="check", hand_id="h1", table_seq=i + 3)) for i in range(140)) +
            line(dict(type="hand_result", hand_id="h1", table_seq=144, shown_cards={"2": ["Ad", "Qh"]})) +
            line(dict(type="hand_start", hand_id="h2", table_seq=145, seat=3)))
        store = LogStore(self.path); store.refresh()
        past = store.view(130)["state"]
        self.assertEqual(past["hole"], ["As", "Kh"])
        self.assertEqual(past["players"][2]["cards"], [])
        self.assertIsNone(past["result"])
        self.assertFalse(any(h["kind"] == "shown" for h in past["history"]))
        self.assertFalse(any("settlement" in h for h in past["history"]))
        self.assertEqual(next(h["cards"] for h in past["history"] if h["kind"] == "hole"), ["As", "Kh"])
        self.assertEqual(store.view(142)["state"]["players"][2]["cards"], ["Ad", "Qh"])
        self.assertEqual(store.view()["state"]["hole"], [])
        self.assertEqual([h["cards"] for h in store.view()["state"]["history"] if h["kind"] == "shown"], [["Ad", "Qh"]])
        self.assertEqual(sum("settlement" in h for h in store.view()["state"]["history"]), 1)
        self.assertEqual(store.view(0)["state"]["hole"], [])
        self.assertEqual(store.view(0)["state"]["history"], [])

    def test_demo_replays_to_consistent_chip_totals(self):
        store = LogStore(Path(__file__).resolve().parents[1] / "table_player/demo.jsonl")
        store.refresh()
        self.assertEqual(store.invalid, 0)
        data = store.view(35)["state"]
        self.assertEqual(data["street"], "showdown")
        self.assertEqual(data["pot"], 1890)
        self.assertEqual(sum(p["stack"] for p in data["players"]), 18000)


class ServerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.root = Path(cls.tmp.name)
        cls.log = cls.root / "openpoker_ws.jsonl"
        cls.log.write_text(line(snapshot()))
        cls.sources = Sources(cls.log, root=cls.root)
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), handler_for(cls.sources))
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown(); cls.server.server_close(); cls.thread.join(); cls.tmp.cleanup()

    def request(self, path, headers=None):
        conn = HTTPConnection("127.0.0.1", self.server.server_port)
        conn.request("GET", path, headers=headers or {})
        response = conn.getresponse()
        result = response.status, response.read()
        conn.close()
        return result

    def test_page_state_and_explicit_source(self):
        self.assertEqual(self.request("/")[0], 200)
        code, raw = self.request("/api/sources")
        self.assertEqual(code, 200)
        source = json.loads(raw)["default"]
        code, raw = self.request(f"/api/state?source={source}&at=live")
        self.assertEqual(code, 200)
        self.assertEqual(json.loads(raw)["state"]["hole"], ["As", "Kh"])

    def test_live_append_is_seen_without_restarting(self):
        with self.log.open("a") as f:
            f.write(line(dict(type="player_action", seat=3, table_seq=3, action="check")))
        code, raw = self.request("/api/state")
        self.assertEqual(code, 200)
        self.assertEqual(json.loads(raw)["state"]["players"][3]["last_action"], "check")

    def test_private_files_and_arbitrary_paths_are_not_served(self):
        for path in ("/.env", "/../.env", "/server.py", "/artifacts/openpoker_ws.jsonl"):
            self.assertEqual(self.request(path)[0], 404)
        self.assertEqual(self.request("/api/state?source=../../.env")[0], 400)
        self.assertEqual(self.request("/api/state?at=invalid")[0], 400)
        self.assertEqual(self.request("/", {"Host": "untrusted.example"})[0], 403)


if __name__ == "__main__":
    unittest.main()

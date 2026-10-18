"""poker_manager client contracts; no checkpoint, token or server required."""

import copy
from datetime import datetime, timezone
import io
import json
import os
from pathlib import Path
import select
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
from poker_manager_play import ManagerSession
from poker_live.decision import DecisionWorker


class Advisor:
    def __init__(self, response=None):
        self.requests = []
        self.response = response or {"action": "call", "source": "test"}
        self.observations = []

    def decide(self, request):
        self.requests.append(copy.deepcopy(request))
        return self.response

    def observe(self, request):
        self.observations.append(request)


def event(kind, seq, **fields):
    return dict(type=kind, stream="event", table_id="t", hand_id="h",
                table_seq=seq, **fields)


def snapshot(seq=4, n=6, dealt=(0, 1, 2), hero=1):
    return dict(type="table_state", stream="state", table_id="t", hand_id="h",
                table_seq=seq, street="preflop", board=[], actor_seat=0,
                seats=[dict(seat=s, name=f"p{s}", stack=2000 - (10 if s == 1 else 20 if s == 2 else 0),
                            bet=10 if s == 1 else 20 if s == 2 else 0,
                            in_hand=s in dealt, folded=False) for s in range(n)],
                hero={"seat": hero, "hole_cards": ["As", "Kh"]})


def action(seq, seat, kind, delta, before=2000):
    return event("player_action", seq, seat=seat, action=kind, amount=delta,
                 amount_mode="incremental", contribution_delta=delta,
                 street="preflop", stack_before=before, stack_after=before-delta)


def turn(seq=9, token="turn", **fields):
    return event("your_turn", seq, turn_token=token,
                 action_deadline=datetime.fromtimestamp(time.time()+30, timezone.utc).isoformat(),
                 valid_actions=fields.pop("valid_actions", [{"action": "fold"},
                     {"action": "call", "amount": 30}, {"action": "raise", "min": 60, "max": 2000}]),
                 **fields)


class ManagerTests(unittest.TestCase):
    def session(self, response=None, **kwargs):
        advisor = Advisor(response)
        session = ManagerSession(DecisionWorker(advisor, sync=True), **kwargs)
        sent = []
        session.attach(sent.append)
        session.handle({"type": "connected"})
        session.handle(event("hand_start", 2, seat=1, dealer_seat=0,
                             blinds={"small_blind": 10, "big_blind": 20}))
        session.handle(event("hole_cards", 3, cards=["As", "Kh"]))
        session.handle(snapshot())
        return session, advisor, sent

    def test_incremental_raise_short_call_allin_and_board(self):
        session, advisor, sent = self.session()
        self.assertEqual(sent, [{"type": "join_lobby", "buy_in": 2000}])
        # SB has already posted 10: putting in 90 raises TO 100, not 90.
        session.handle(action(5, 0, "raise", 40))
        session.handle(action(6, 1, "raise", 90, before=1990))
        session.handle(action(7, 2, "call", 80, before=1980))
        session.handle(turn())
        req = advisor.requests[-1]
        self.assertEqual(req["actions"][1], {"seat": 1, "action": "raise", "to": 100})
        self.assertEqual(req["stacks"], [2000]*6)
        self.assertEqual(req["dealt_seats"], [0, 1, 2])
        for seq, street, cards in [(10, "flop", ["2c", "3d", "4s"]), (11, "turn", ["9h"]),
                                   (12, "river", ["Jc"])]:
            session.handle(event("community_cards", seq, street=street, cards=cards))
        self.assertEqual(session.hand.board, ["2c", "3d", "4s", "9h", "Jc"])
        self.assertEqual(session.hand.bets, {0: 0, 1: 0, 2: 0})
        # A capped call uses the explicit delta, not the table's highest bet.
        session.hand.street = "preflop"
        session.handle(action(13, 1, "call", 1900, before=1900))
        self.assertEqual(session.hand.stacks[1], 0)
        session.handle(action(14, 2, "all_in", 1900, before=1900))
        self.assertEqual(session.hand.stacks[2], 0)
        self.assertEqual(session.hand.starts[2], 2000)

    def test_snapshots_never_authorize_and_tokens_are_deduplicated(self):
        session, advisor, sent = self.session()
        snap = snapshot(5)
        snap.update(actor_seat=None, waiting_reason="action_delay", next_actor_seat=1)
        snap["hero"].update(valid_actions=turn()["valid_actions"])
        session.handle(snap)
        self.assertFalse(advisor.requests)
        session.handle(turn())
        session.handle(turn(10))
        self.assertEqual(len(advisor.requests), 1)
        self.assertEqual(len([m for m in sent if m["type"] == "action"]), 1)

    def test_reconnect_replays_before_snapshot_and_buffers_live_messages(self):
        session, advisor, sent = self.session()
        session.handle(action(5, 0, "raise", 40))
        session.disconnect()
        session.attach(sent.append)
        session.handle({"type": "connected", "table_id": "t"})
        self.assertEqual(sent[-1], {"type": "resync_request", "table_id": "t", "last_table_seq": 5})
        missing = action(8, 2, "call", 20, before=1980)
        snap = snapshot(15)
        snap["seats"][0].update(stack=1960, bet=40)
        snap["seats"][2].update(stack=1960, bet=40)
        snap["actor_seat"] = 1
        snap["hero"].update({k: v for k, v in turn().items()
                             if k in ("turn_token", "valid_actions", "action_deadline")})
        session.handle(snap)
        session.handle(turn(16))
        session.handle(dict(type="resync_response", table_id="t", to_table_seq=20,
                            snapshot=snap, replayed_events=[missing, action(5, 0, "raise", 40)]))
        self.assertFalse(session.hand.invalid)
        self.assertEqual(len(advisor.requests), 1)
        self.assertEqual(len(advisor.requests[0]["actions"]), 2)
        self.assertEqual(session.hand.stacks[2], 1960)
        self.assertEqual(session.last_seq, 20)

    def test_cold_reconnect_and_truncated_replay_use_safe_action(self):
        for truncated in (True, False):
            with self.subTest(truncated=truncated):
                session, advisor, sent = self.session()
                if not truncated:
                    session.hand = None
                session.handle({"type": "connected", "table_id": "t"})
                snap = snapshot()
                snap["hero"].update({k: v for k, v in turn().items()
                                     if k in ("turn_token", "valid_actions", "action_deadline")})
                session.handle(dict(type="resync_response", table_id="t", to_table_seq=20,
                    replay_truncated=truncated, replayed_events=[], snapshot=snap))
                self.assertFalse(advisor.requests)
                self.assertEqual(sent[-1]["action"], "fold")

    def test_reconnect_retries_identical_payload_only_if_token_unchanged(self):
        session, advisor, sent = self.session()
        session.handle(turn())
        original = dict(sent[-1])
        session.disconnect()
        session.attach(lambda m: sent.append(m))
        session.handle({"type": "connected", "table_id": "t"})
        snap = snapshot()
        snap["hero"].update({k: v for k, v in turn().items()
                             if k in ("turn_token", "valid_actions", "action_deadline")})
        session.handle(dict(type="resync_response", table_id="t", to_table_seq=20,
                            replayed_events=[], snapshot=snap))
        self.assertEqual(sent[-1], original)
        self.assertEqual(len(advisor.requests), 1)

    def test_hu_is_explicit_safe_fallback_and_nine_seat_config_works(self):
        session, advisor, sent = self.session()
        session.hand.dealt = [0, 1]
        session.handle(turn())
        self.assertFalse(advisor.requests)
        self.assertEqual(sent[-1]["action"], "fold")
        session = ManagerSession(DecisionWorker(advisor, sync=True), n_seats=9)
        session.attach(sent.append)
        session.handle(event("hand_start", 2, seat=1, dealer_seat=0,
                             blinds={"small_blind": 5, "big_blind": 10}))
        snap = snapshot(n=9)
        snap["seats"][1].update(stack=1995, bet=5)
        snap["seats"][2].update(stack=1990, bet=10)
        session.handle(snap)
        session.handle(turn())
        self.assertEqual(advisor.requests[-1]["num_seats"], 9)
        self.assertEqual(advisor.requests[-1]["big_blind"], 10)

    def test_deadline_fallback_and_late_result_discard(self):
        session, advisor, sent = self.session()
        callbacks = []
        session.worker.submit_decide = lambda req, callback: callbacks.append(callback)
        session.handle(turn())
        session.pending["deadline"] = time.time()+0.25
        session.tick()
        self.assertEqual(sent[-1]["action"], "fold")
        count = len(sent)
        callbacks[0]({"action": "call"}, 1000)
        self.assertEqual(len(sent), count)
        session.handle(turn(11, "next"))
        session.disconnect()
        callbacks[-1]({"action": "call"}, 1000)
        self.assertEqual(len(sent), count)

    def test_illegal_raise_is_not_sent_and_integral_raise_is_integer(self):
        for amount, expected in [(60.5, "fold"), (60.0, "raise"), (9999, "fold")]:
            session, _, sent = self.session({"action": "raise", "amount": amount})
            session.handle(turn())
            self.assertEqual(sent[-1]["action"], expected)
            if expected == "raise":
                self.assertIs(type(sent[-1]["amount"]), int)

    def test_completion_logs_real_result_and_leaves_once(self):
        log = io.StringIO()
        session, _, sent = self.session(num_hands=1, hh_log=log)
        result = event("hand_result", 10, final_stacks={"0": 2020, "1": 1990, "2": 1990},
                       total_pot=30, payouts=[{"seat": 0, "amount": 30}])
        session.handle(result)
        session.handle(result)
        self.assertTrue(session.done)
        self.assertEqual(session.hands, 1)
        self.assertEqual(sent[-1], {"type": "leave_table"})
        self.assertEqual(json.loads(log.getvalue())["result"], result)

    def test_mismatched_seats_and_auth_failure_stop(self):
        session, _, _ = self.session()
        session.handle(snapshot(n=9))
        self.assertTrue(session.done)
        self.assertIn("server seats=9", session.error)
        session = ManagerSession(None)
        session.handle({"type": "error", "code": "auth_failed"})
        self.assertTrue(session.done)


@unittest.skipUnless(os.environ.get("POKER_MANAGER_PROJECT"), "set POKER_MANAGER_PROJECT for local WS integration")
class ManagerLiveTests(unittest.TestCase):
    def test_cli_against_real_service(self):
        root = Path(__file__).resolve().parents[1]
        server = subprocess.Popen(["node", str(root / "tests/common/poker_manager_server.mjs"),
                                   os.environ["POKER_MANAGER_PROJECT"]],
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            self.assertTrue(select.select([server.stdout], [], [], 10)[0], "server startup timeout")
            port = server.stdout.readline().strip()
            self.assertTrue(port.isdigit(), "server failed to start")
            with tempfile.TemporaryDirectory() as directory:
                directory = Path(directory)
                stub = directory / "advisor"
                stub.write_text(f"#!{sys.executable}\n" + textwrap.dedent('''\
                    import json, sys
                    print(json.dumps({"ready": True, "n_seats": 6}), flush=True)
                    for line in sys.stdin:
                        req = json.loads(line)
                        v = req["valid"]
                        action = "check" if v["can_check"] else "call" if v["can_call"] else "fold"
                        print(json.dumps({"action": action, "source": "test"}), flush=True)
                    '''))
                stub.chmod(0o700)
                actions, hands = directory / "actions.jsonl", directory / "hands.jsonl"
                completed = subprocess.run([sys.executable, str(root / "tools/poker_manager_play.py"),
                    "--url", f"ws://127.0.0.1:{port}/ws", "--token", "test-hero",
                    "--advisor-bin", str(stub), "--checkpoint", "stub", "--bucket-table", "stub",
                    "--num-hands", "3", "--action-log", str(actions), "--hh-log", str(hands)],
                    capture_output=True, text=True, timeout=30)
                self.assertEqual(completed.returncode, 0, completed.stderr)
                decisions = [json.loads(line) for line in actions.read_text().splitlines()]
                history = [json.loads(line) for line in hands.read_text().splitlines()]
                self.assertEqual(len(history), 3)
                self.assertTrue(all(h["complete"] and len(h["board"]) == 5 for h in history))
                self.assertGreater(len(decisions), 6)
                self.assertTrue(all(d["resp"]["source"] == "test" for d in decisions), completed.stderr)
                self.assertTrue(any(a["action"] == "raise" for d in decisions for a in d["req"]["actions"]))
        finally:
            server.terminate()
            try:
                server.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
                server.communicate()


if __name__ == "__main__":
    unittest.main()

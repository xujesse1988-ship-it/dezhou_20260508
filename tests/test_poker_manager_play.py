"""poker_manager client contracts; no checkpoint, token or server required."""

import contextlib
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
from types import SimpleNamespace
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))
from poker_manager_play import ManagerSession, run
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

    def test_six_seat_table_matches_nine_seat_table_with_empty_extra_seats(self):
        requests = []
        for capacity in (6, 9):
            advisor = Advisor()
            session = ManagerSession(DecisionWorker(advisor, sync=True), n_seats=9)
            sent = []
            session.attach(sent.append)
            session.handle(event("hand_start", 2, seat=1, dealer_seat=0,
                                 blinds={"small_blind": 10, "big_blind": 20}))
            session.handle(snapshot(n=capacity, dealt=tuple(range(6))))
            for message in (action(5, 3, "raise", 40), action(6, 4, "call", 40),
                            action(7, 5, "fold", 0), action(8, 0, "call", 40)):
                session.handle(message)
            session.handle(turn())
            self.assertFalse(session.done, session.error)
            self.assertEqual(sent[-1]["action"], "call")
            requests.append(advisor.requests[-1])
        self.assertEqual(requests[0], requests[1])
        self.assertEqual(requests[0]["num_seats"], 9)
        self.assertEqual(requests[0]["stacks"], [2000] * 9)
        self.assertEqual(requests[0]["dealt_seats"], list(range(6)))

    def test_two_seat_table_is_accepted_with_explicit_hu_fallback(self):
        advisor = Advisor()
        session = ManagerSession(DecisionWorker(advisor, sync=True), n_seats=9)
        sent = []
        session.attach(sent.append)
        session.handle(event("hand_start", 2, seat=1, dealer_seat=0,
                             blinds={"small_blind": 10, "big_blind": 20}))
        snap = snapshot(n=2, dealt=(0, 1))
        snap["seats"][0].update(stack=1990, bet=10)
        snap["seats"][1].update(stack=1980, bet=20)
        session.handle(snap)
        session.handle(turn())
        self.assertFalse(session.done)
        self.assertFalse(advisor.requests)
        self.assertEqual(sent[-1]["action"], "fold")

    def test_out_of_range_server_seat_is_rejected(self):
        session, _, _ = self.session()
        snap = snapshot()
        snap["seats"][-1]["seat"] = 8
        session.handle(snap)
        self.assertTrue(session.done)

    def test_illegal_raise_is_not_sent_and_integral_raise_is_integer(self):
        for amount, expected in [(60.5, "fold"), (60.0, "raise"), (9999, "fold")]:
            session, _, sent = self.session({"action": "raise", "amount": amount})
            session.handle(turn())
            self.assertEqual(sent[-1]["action"], expected)
            if expected == "raise":
                self.assertIs(type(sent[-1]["amount"]), int)

    def test_bare_allin_without_allin_option_calls(self):
        # 8db1dbc5: opponent shoved, hero covers -> only fold/call offered; the advisor's bare
        # all_in used to be rejected as illegal and AA was folded.
        fold_call = [{"action": "fold"}, {"action": "call", "amount": 1618}]
        log = io.StringIO()
        session, _, sent = self.session({"action": "all_in", "source": "blueprint"}, action_log=log)
        session.handle(turn(valid_actions=fold_call))
        self.assertEqual(sent[-1]["action"], "call")
        self.assertNotIn("amount", sent[-1])
        self.assertEqual(json.loads(log.getvalue())["resp"]["source"], "blueprint:allin_as_call")
        # all_in offered -> sent as is.
        session, _, sent = self.session({"action": "all_in", "source": "blueprint"})
        session.handle(turn(valid_actions=fold_call + [{"action": "all_in"}]))
        self.assertEqual(sent[-1]["action"], "all_in")
        # neither all_in nor call (check spot) -> still the safe fallback, original reply logged.
        log = io.StringIO()
        session, _, sent = self.session({"action": "all_in", "source": "blueprint"}, action_log=log)
        session.handle(turn(valid_actions=[{"action": "check"}]))
        self.assertEqual(sent[-1]["action"], "check")
        resp = json.loads(log.getvalue())["resp"]
        self.assertEqual(resp["source"], "fallback:illegal_advisor_action")
        self.assertEqual(resp["rejected"], {"action": "all_in", "amount": None, "source": "blueprint"})

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

    def test_exploit_4bet_sends_names_and_observe_with_amounts(self):
        session, advisor, _ = self.session(exploit_4bet=True)
        session.handle(action(5, 0, "raise", 40))
        session.handle(turn())
        self.assertEqual(advisor.requests[-1]["names"], {0: "p0", 1: "p1", 2: "p2"})
        session.handle(event("hand_result", 10, final_stacks={"0": 2050, "1": 1990, "2": 1980},
                             total_pot=70, payouts=[{"seat": 0, "amount": 70}]))
        self.assertEqual(len(advisor.observations), 1)
        obs = advisor.observations[0]
        self.assertEqual(obs["names"], {0: "p0", 1: "p1", 2: "p2"})
        self.assertEqual(obs["actions"], [{"seat": 0, "street": "preflop", "action": "raise",
                                           "contribution_delta": 40, "to_call_before": None}])

    def test_exploit_off_keeps_ipc_unchanged(self):
        session, advisor, _ = self.session()
        session.handle(action(5, 0, "raise", 40))
        session.handle(turn())
        self.assertNotIn("names", advisor.requests[-1])
        session.handle(event("hand_result", 10, final_stacks={"0": 2050, "1": 1990, "2": 1980},
                             total_pot=70, payouts=[{"seat": 0, "amount": 70}]))
        self.assertEqual(advisor.observations, [])
        # --exploit alone keeps the old observe format (no amounts).
        session, advisor, _ = self.session(exploit=True)
        session.handle(action(5, 0, "raise", 40))
        session.handle(event("hand_result", 10, final_stacks={"0": 2050, "1": 1990, "2": 1980},
                             total_pot=70, payouts=[{"seat": 0, "amount": 70}]))
        self.assertEqual(advisor.observations[0]["actions"],
                         [{"seat": 0, "street": "preflop", "action": "raise"}])

    def test_exploit_4bet_flags_pass_through(self):
        from poker_manager_play import fourbet_extra_args
        off = SimpleNamespace(exploit_4bet="off", exploit_4bet_convert=None,
                              exploit_4bet_fold_decay=None, exploit_4bet_3bet_decay=None,
                              exploit_4bet_state=None)
        self.assertEqual(fourbet_extra_args(off), [])
        on = SimpleNamespace(exploit_4bet="on", exploit_4bet_convert=0.5,
                             exploit_4bet_fold_decay=None, exploit_4bet_3bet_decay=0.95,
                             exploit_4bet_state=None)
        self.assertEqual(fourbet_extra_args(on), ["--exploit-4bet", "on", "--exploit-4bet-convert",
                                                  "0.5", "--exploit-4bet-3bet-decay", "0.95"])
        state = SimpleNamespace(exploit_4bet="on", exploit_4bet_convert=None,
                                exploit_4bet_fold_decay=None, exploit_4bet_3bet_decay=None,
                                exploit_4bet_state="state/fourbet.json")
        self.assertEqual(fourbet_extra_args(state),
                         ["--exploit-4bet", "on", "--exploit-4bet-state",
                          os.path.abspath("state/fourbet.json")])

    def test_exploit_4bet_state_requires_main_switch(self):
        from poker_manager_play import main
        err = io.StringIO()
        with self.assertRaises(SystemExit), contextlib.redirect_stderr(err):
            main(["--token", "t", "--checkpoint", "x.ckpt", "--bucket-table", "t.bin",
                  "--exploit-4bet-state", "s.json"])
        self.assertIn("--exploit-4bet-* 子旗需配 --exploit-4bet on", err.getvalue())

    def test_mismatched_seats_and_auth_failure_stop(self):
        session, _, _ = self.session()
        session.handle(snapshot(n=9))
        self.assertTrue(session.done)
        self.assertIn("server seats=9", session.error)
        session = ManagerSession(None)
        session.handle({"type": "error", "code": "auth_failed"})
        self.assertTrue(session.done)

    def test_default_busted_stops_without_rebuy(self):
        session, _, sent = self.session()
        count = len(sent)
        session.handle(event("busted", 10))
        self.assertTrue(session.done)
        self.assertEqual(len(sent), count)

    def test_auto_rebuy_rejoins_after_removal_and_keeps_hand_limit(self):
        session, _, sent = self.session(auto_rebuy=True, num_hands=2,
            read_account=lambda: {"chip_balance": 8000, "auto_rebuy": True})
        session.handle({"type": "lobby_joined"})
        self.assertEqual(sent[-1], {"type": "set_auto_rebuy", "enabled": True})
        session.handle(event("hand_result", 10, final_stacks={"1": 0}))
        count = len(sent)
        session.handle(event("busted", 11))
        self.assertFalse(session.done)
        self.assertEqual(len(sent), count)
        session.handle(event("player_left", 12, seat=1, reason="busted"))
        self.assertEqual(sent[-1], {"type": "join_lobby", "buy_in": 2000})
        self.assertEqual(session.hands, 1)
        self.assertIsNone(session.hand)
        self.assertIsNone(session.table_id)
        session.handle(dict(event("hand_start", 2, seat=1, dealer_seat=0,
            blinds={"small_blind": 10, "big_blind": 20}), table_id="new", hand_id="next"))
        session.handle(dict(event("hand_result", 3, final_stacks={"1": 0}),
                            table_id="new", hand_id="next"))
        self.assertTrue(session.done)
        self.assertEqual(session.hands, 2)
        self.assertEqual(sent[-1], {"type": "leave_table"})
        count = len(sent)
        session.handle(event("busted", 4))
        session.handle({"type": "rebuy_confirmed", "chip_balance": 1500})
        self.assertEqual(len(sent), count)

    def test_zero_balance_waits_for_confirmation_and_respects_available_buy_in(self):
        session = ManagerSession(None, auto_rebuy=True,
            read_account=lambda: {"chip_balance": 0, "auto_rebuy": True})
        sent = []
        session.attach(sent.append)
        session.handle({"type": "connected"})
        self.assertEqual(sent, [{"type": "set_auto_rebuy", "enabled": True}])
        session.handle({"type": "auto_rebuy_set", "enabled": True})
        session.handle({"type": "auto_rebuy_scheduled", "rebuy_at": "later"})
        self.assertEqual(len(sent), 1)
        confirmation = {"type": "rebuy_confirmed", "chip_balance": 1500}
        session.handle(confirmation)
        self.assertEqual(sent[-1], {"type": "join_lobby", "buy_in": 1500})
        session.handle(confirmation)
        self.assertEqual(len(sent), 2)
        self.assertFalse(session.done)

    def test_small_balance_rebuy_retries_same_id_after_disconnect(self):
        session = ManagerSession(None, auto_rebuy=True,
            read_account=lambda: {"chip_balance": 500, "auto_rebuy": True})
        sent = []
        session.attach(sent.append)
        session.handle({"type": "connected"})
        request = dict(sent[-1])
        self.assertEqual(request["type"], "rebuy")
        self.assertEqual(request["amount"], 0)
        session.disconnect()
        session.attach(sent.append)
        session.handle({"type": "connected"})
        self.assertEqual(sent[-1], request)
        session.handle({"type": "rebuy_confirmed", "chip_balance": 2000,
                        "request_id": request["request_id"], "replayed": True})
        self.assertEqual(sent[-1], {"type": "join_lobby", "buy_in": 2000})
        self.assertIsNone(session.rebuy_request_id)

    def test_auto_rebuy_cold_reconnect_uses_current_account_balance(self):
        session = ManagerSession(None, auto_rebuy=True,
            read_account=lambda: {"chip_balance": 1500, "auto_rebuy": True})
        sent = []
        session.attach(sent.append)
        session.handle({"type": "connected"})
        self.assertEqual(sent, [{"type": "join_lobby", "buy_in": 1500}])

    def test_auto_rebuy_does_not_rejoin_unrelated_removal(self):
        session, _, sent = self.session(auto_rebuy=True,
            read_account=lambda: {"chip_balance": 8000})
        count = len(sent)
        session.handle(event("player_left", 10, seat=1, reason="away"))
        self.assertTrue(session.done)
        self.assertEqual(len(sent), count)

    def test_stack_drift_leaves_once_then_rejoins_at_buy_in(self):
        for stack in (3620, 1580):
            session, _, sent = self.session()
            session.handle(event("hand_result", 10, final_stacks={"0": 2000, "1": stack}))
            self.assertEqual(sent[-1], {"type": "leave_table"})
            self.assertTrue(session.leaving)
            # Leave is pending until the next hand ends; it must not be sent again.
            session.handle(dict(event("hand_start", 11, seat=1, dealer_seat=1,
                blinds={"small_blind": 10, "big_blind": 20}), hand_id="h2"))
            session.handle(dict(event("hand_result", 12, final_stacks={"1": stack}), hand_id="h2"))
            self.assertEqual(sum(m["type"] == "leave_table" for m in sent), 1)
            session.handle(event("player_left", 13, seat=1, reason="left"))
            self.assertEqual(sent[-1], {"type": "join_lobby", "buy_in": 2000})
            self.assertFalse(session.done)
            self.assertFalse(session.leaving)
            self.assertIsNone(session.table_id)
            self.assertEqual(session.hands, 2)

    def test_stack_inside_band_or_band_off_stays_seated(self):
        for band, stack in (((80, 180), 1600), ((80, 180), 3600), (None, 9000)):
            session, _, sent = self.session(stack_band_bb=band)
            count = len(sent)
            session.handle(event("hand_result", 10, final_stacks={"1": stack}))
            self.assertEqual(len(sent), count)
            self.assertFalse(session.leaving)

    def test_not_at_table_after_leave_rejoins(self):
        session, _, sent = self.session()
        session.handle(event("hand_result", 10, final_stacks={"1": 4000}))
        session.handle({"type": "error", "code": "not_at_table"})
        self.assertEqual(sent[-1], {"type": "join_lobby", "buy_in": 2000})
        self.assertFalse(session.done)

    def test_out_of_band_seat_does_not_leave_every_hand(self):
        # An auto-rebuy grant of 1500 (75BB) can only buy in below the band.
        session, _, sent = self.session(auto_rebuy=True,
            read_account=lambda: {"chip_balance": 1500, "auto_rebuy": True})
        session.handle(dict(type="table_joined", seat=1, table_id="t",
                            players=[{"seat": 0, "stack": 2000}, {"seat": 1, "stack": 1500}]))
        count = len(sent)
        session.handle(event("hand_result", 10, final_stacks={"1": 1400}))
        self.assertEqual(len(sent), count)

    def test_reconnect_while_leaving_resends_leave(self):
        session, _, sent = self.session()
        session.handle(event("hand_result", 10, final_stacks={"1": 4000}))
        count = len(sent)
        session.disconnect()
        session.attach(sent.append)
        session.handle({"type": "connected", "table_id": "t"})
        self.assertEqual([m["type"] for m in sent[count:]], ["leave_table", "resync_request"])
        self.assertTrue(session.leaving)

    def test_auto_rebuy_seated_reconnect_handles_unsequenced_confirmation(self):
        session, _, sent = self.session(auto_rebuy=True,
            read_account=lambda: {"chip_balance": 8000})
        count = len(sent)
        session.disconnect()
        session.attach(sent.append)
        session.handle({"type": "connected", "table_id": "t"})
        self.assertEqual([m["type"] for m in sent[count:]],
                         ["set_auto_rebuy", "resync_request"])
        session.handle({"type": "auto_rebuy_set", "enabled": False})
        session.handle(dict(type="resync_response", table_id="t", to_table_seq=20,
                            replayed_events=[], snapshot=snapshot()))
        self.assertTrue(session.done)
        self.assertEqual(session.error, "server did not enable auto rebuy")


class ManagerTransportTests(unittest.TestCase):
    class Timeout(Exception):
        pass

    def setUp(self):
        self.now = 0
        self.sleeps = []
        self.sockets = []
        self.ws_log = SimpleNamespace(conn=0, event=lambda kind, **kw: self.events.append(kind),
                                      msg=lambda *args: None)
        self.events = []
        self.websocket = SimpleNamespace(
            ABNF=SimpleNamespace(OPCODE_TEXT=1, OPCODE_CLOSE=8, OPCODE_PING=9, OPCODE_PONG=10),
            WebSocketTimeoutException=self.Timeout, WebSocketException=self.Timeout)

    def socket(self, frames):
        owner = self

        class Socket:
            def __init__(self):
                self.frames = iter(frames)
                self.sent = []
                self.pings = []
                self.closed = False

            def settimeout(self, timeout):
                pass

            def send(self, raw):
                self.sent.append(json.loads(raw))

            def ping(self, payload):
                self.pings.append(payload)

            def recv_data(self, control_frame=False):
                owner.assertTrue(control_frame)
                delay, frame = next(self.frames)
                owner.now += delay
                if frame is None:
                    raise owner.Timeout()
                if isinstance(frame, dict):
                    return 1, json.dumps(frame)
                return frame

            def close(self):
                self.closed = True

        socket = Socket()
        self.sockets.append(socket)
        return socket

    def finish_hand(self):
        return [(0, event("hand_start", 2, seat=1, dealer_seat=0,
                           blinds={"small_blind": 10, "big_blind": 20})),
                (0, event("hand_result", 3, final_stacks={"1": 2000}))]

    def play(self, session):
        def sleep(seconds):
            self.now += seconds
            self.sleeps.append(seconds)

        with patch.dict(sys.modules, {"websocket": self.websocket}), \
                patch.object(self.websocket, "create_connection", create=True,
                             side_effect=self.sockets) as connect, \
                patch("poker_manager_play.time.monotonic", side_effect=lambda: self.now), \
                patch("poker_manager_play.time.sleep", side_effect=sleep):
            run(session, "ws://example.invalid/ws", "test-token", self.ws_log)
        return connect.call_count

    def test_missing_matching_pong_reconnects_and_keeps_hand_count(self):
        first = self.socket([(0, {"type": "connected"}), (15, None),
                             (5, (10, b"unrelated-pong")), (5, None)])
        second = self.socket([(0, {"type": "connected"}), *self.finish_hand()])
        session = ManagerSession(None, num_hands=44)
        session.hands = 43
        self.assertEqual(self.play(session), 2)
        self.assertEqual(session.hands, 44)
        self.assertEqual(first.sent, [{"type": "join_lobby", "buy_in": 2000}])
        self.assertEqual(second.sent, [{"type": "join_lobby", "buy_in": 2000},
                                       {"type": "leave_table"}])
        self.assertTrue(first.closed and second.closed)
        self.assertIn("heartbeat_timeout", self.events)
        self.assertEqual(self.sleeps, [3])

    def test_matching_pongs_keep_quiet_connection_alive(self):
        socket = self.socket([(0, {"type": "connected"}), (15, None),
                              (1, (10, b"poker-manager-heartbeat")), (15, None),
                              (1, (10, b"poker-manager-heartbeat")), *self.finish_hand()])
        self.assertEqual(self.play(ManagerSession(None, num_hands=1)), 1)
        self.assertEqual(len(socket.pings), 2)
        self.assertEqual(self.events.count("heartbeat_pong"), 2)
        self.assertNotIn("heartbeat_timeout", self.events)
        self.assertFalse(self.sleeps)

    def test_server_pings_are_liveness_and_do_not_enter_json_handler(self):
        socket = self.socket([(0, {"type": "connected"}),
                              *[(10, (9, b"server-ping"))] * 4, *self.finish_hand()])
        self.assertEqual(self.play(ManagerSession(None, num_hands=1)), 1)
        self.assertFalse(socket.pings)
        self.assertFalse(self.sleeps)

    def test_replaced_session_still_stops_without_reconnecting(self):
        socket = self.socket([(0, (8, (4002).to_bytes(2, "big")))])
        with self.assertRaisesRegex(RuntimeError, "websocket closed: 4002"):
            self.play(ManagerSession(None))
        self.assertTrue(socket.closed)
        self.assertEqual(self.ws_log.conn, 1)
        self.assertFalse(self.sleeps)


@unittest.skipUnless(os.environ.get("POKER_MANAGER_PROJECT"), "set POKER_MANAGER_PROJECT for local WS integration")
class ManagerLiveTests(unittest.TestCase):
    def test_cli_against_real_service(self):
        self.run_cli(False)

    def test_auto_rebuy_against_real_service(self):
        self.run_cli(True)

    def run_cli(self, auto_rebuy):
        root = Path(__file__).resolve().parents[1]
        server = subprocess.Popen(["node", str(root / "tests/common/poker_manager_server.mjs"),
                                   os.environ["POKER_MANAGER_PROJECT"],
                                   "rebuy" if auto_rebuy else "normal"],
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
                    print(json.dumps({"ready": True, "n_seats": 9}), flush=True)
                    for line in sys.stdin:
                        req = json.loads(line)
                        v = req["valid"]
                        action = "check" if v["can_check"] else "call" if v["can_call"] else "fold"
                        print(json.dumps({"action": action, "source": "test"}), flush=True)
                    '''))
                stub.chmod(0o700)
                actions, hands = directory / "actions.jsonl", directory / "hands.jsonl"
                ws_log = directory / "ws.jsonl"
                completed = subprocess.run([sys.executable, str(root / "tools/poker_manager_play.py"),
                    "--url", f"ws://127.0.0.1:{port}/ws", "--token", "test-hero",
                    "--advisor-bin", str(stub), "--checkpoint", "stub", "--bucket-table", "stub",
                    "--seats", "9", *(["--auto-rebuy"] if auto_rebuy else []),
                    "--ws-log", str(ws_log),
                    "--num-hands", "3", "--action-log", str(actions), "--hh-log", str(hands)],
                    capture_output=True, text=True, timeout=30)
                self.assertEqual(completed.returncode, 0, completed.stderr)
                decisions = [json.loads(line) for line in actions.read_text().splitlines()]
                history = [json.loads(line) for line in hands.read_text().splitlines()]
                self.assertEqual(len(history), 3)
                self.assertTrue(all(h["complete"] and len(h["board"]) == 5 for h in history))
                self.assertGreater(len(decisions), 6)
                self.assertTrue(all(d["resp"]["source"] == "test" for d in decisions), completed.stderr)
                self.assertTrue(all(d["req"]["num_seats"] == 9 and len(d["req"]["stacks"]) == 9
                                    and d["req"]["dealt_seats"] == [0, 1, 2] for d in decisions))
                self.assertTrue(any(a["action"] == "raise" for d in decisions for a in d["req"]["actions"]))
                if auto_rebuy:
                    events = [json.loads(row["raw"]) for line in ws_log.read_text().splitlines()
                              if (row := json.loads(line)).get("dir") in ("in", "out")]
                    self.assertEqual([e["buy_in"] for e in events if e["type"] == "join_lobby"],
                                     [2000, 1500])
                    for kind in ("busted", "player_left", "auto_rebuy_scheduled", "rebuy_confirmed"):
                        self.assertTrue(any(e["type"] == kind for e in events), kind)
                    self.assertTrue(any(e["type"] == "auto_rebuy_set" and e["enabled"] for e in events))
                    self.assertEqual(sum(e["type"] == "leave_table" for e in events), 1)
        finally:
            server.terminate()
            try:
                server.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
                server.communicate()


if __name__ == "__main__":
    unittest.main()

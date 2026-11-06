"""Tests for the local arena table. Run: python3 -m unittest tools/wopo_arena/test_arena.py

The end-to-end tests run real tools/wopo_play.py drivers against the table with a random advisor. They
need websocket-client. Each driver re-checks every message with its own engine, so a run with no voided
hand shows that the two rule implementations agree.
"""

import collections
import json
import os
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import threading
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(REPO, "tools"))

import arena  # noqa: E402
from engine import Hand, IllegalAction, settle_pots  # noqa: E402
from opponent_analysis.history import build_hand  # noqa: E402
import report  # noqa: E402

CARDS = ["Ah", "Ad", "Kh", "Kd", "Qh", "Qd", "Jh", "Jd", "9c", "9d", "8c", "8d", "7c", "7d", "6c", "6d"]
BOARD = ["2c", "3d", "5h", "Ts", "4s"]


def holes(n=8):
    return [CARDS[i * 2:i * 2 + 2] for i in range(n)]


def new_hand(stacks=None, button=7):
    return Hand(stacks or [20000] * 8, button, 50, 100, 100, holes(), BOARD)


def fold_to(h, seat):
    while h.to_act != seat:
        h.apply("fold")


class HandSetup(unittest.TestCase):
    def test_antes_blinds_and_first_actor(self):
        h = new_hand()
        self.assertEqual(h.pot, 950)
        self.assertEqual((h.sb_seat, h.bb_seat, h.to_act), (0, 1, 2))
        self.assertEqual(h.bet, [50, 100, 0, 0, 0, 0, 0, 0])
        self.assertEqual(h.stack[0], 20000 - 150)
        self.assertEqual(h.stack[1], 20000 - 200)
        self.assertEqual(h.stack[2], 20000 - 100)

    def test_seat_that_cannot_post_is_rejected(self):
        self.assertEqual(new_hand([200] + [20000] * 7).stack[0], 50, "SB: ante + 50 leaves 50")
        with self.assertRaises(ValueError):
            new_hand([20000, 200] + [20000] * 6)  # BB: ante + 100 leaves nothing

    def test_first_legal_range(self):
        self.assertEqual(new_hand().legal(), {"can_check": False, "can_call": True, "can_raise": True,
                                              "min_raise": 200, "max_raise": 19900})


class Betting(unittest.TestCase):
    def test_big_blind_keeps_option_after_limps(self):
        h = new_hand()
        for _ in range(6):
            h.apply("call")
        self.assertEqual(h.to_act, 0)
        h.apply("call")
        self.assertEqual(h.to_act, 1)
        self.assertEqual(h.legal()["can_check"], True)
        self.assertEqual(h.legal()["can_raise"], True)
        h.apply("check")
        self.assertTrue(h.round_closed())

    def test_min_raise_follows_last_full_raise(self):
        h = new_hand()
        h.apply("raise", 500)  # raise size 400
        self.assertEqual(h.legal()["min_raise"], 900)
        h.apply("raise", 1000)  # raise size 500
        self.assertEqual(h.legal()["min_raise"], 1500)

    def test_raise_below_minimum_is_illegal(self):
        h = new_hand()
        h.apply("raise", 500)
        with self.assertRaises(IllegalAction):
            h.apply("raise", 899)

    def test_short_all_in_does_not_reopen(self):
        # Seat 4 is short: raising 500 -> 700 is only 200 more, below the 400 minimum.
        stacks = [20000] * 8
        stacks[3] = 800
        h = new_hand(stacks)
        h.apply("raise", 500)  # seat 2
        act = h.apply("all_in")  # seat 3: total 700
        self.assertEqual((act["label"], act["to"], act["action"], act["amount_mode"]), ("all_in", 700, "all_in", "incremental"))
        self.assertEqual(h.last_raise, 400)
        for _ in range(4):
            h.apply("fold")  # seats 4-7
        h.apply("fold")  # seat 0
        h.apply("call")  # seat 1 had not acted: calls 700, may have raised
        self.assertEqual(h.to_act, 2)
        legal = h.legal()
        self.assertFalse(legal["can_raise"], "seat 2 only faces a short all-in")
        self.assertTrue(legal["can_call"])

    def test_seat_yet_to_act_may_raise_over_short_all_in(self):
        stacks = [20000] * 8
        stacks[3] = 800
        h = new_hand(stacks)
        h.apply("raise", 500)
        h.apply("all_in")
        legal = h.legal()  # seat 4 has not acted
        self.assertTrue(legal["can_raise"])
        self.assertEqual(legal["min_raise"], 1100, "700 + last full raise 400")

    def test_call_for_less_is_call_label_all_in_wire(self):
        stacks = [20000] * 8
        stacks[3] = 400
        h = new_hand(stacks)
        h.apply("raise", 500)
        act = h.apply("call")
        self.assertEqual((act["label"], act["action"], act["delta"], act["to"]), ("call", "all_in", 300, None))

    def test_postflop_order_and_reset(self):
        h = new_hand()
        h.apply("raise", 300)
        fold_to(h, 1)
        h.apply("call")
        self.assertTrue(h.round_closed())
        self.assertEqual(h.next_street(), BOARD[:3])
        self.assertEqual(h.to_act, 1, "first live seat after the button")
        self.assertEqual(h.legal()["min_raise"], 100)
        h.apply("check")
        self.assertEqual(h.to_act, 2)

    def test_no_action_when_only_one_can_bet(self):
        stacks = [20000] * 8
        stacks[2] = 1000
        h = new_hand(stacks)
        h.apply("all_in")  # seat 2: 900
        fold_to(h, 1)
        h.apply("call")
        self.assertTrue(h.round_closed())
        for _ in range(3):
            h.next_street()
            self.assertIsNone(h.to_act)
        self.assertTrue(h.needs_showdown())


class Settlement(unittest.TestCase):
    def test_fold_win_refunds_uncalled_bet(self):
        h = new_hand()
        h.apply("raise", 1000)  # seat 2
        for _ in range(7):
            h.apply("fold")
        r = h.settle()
        self.assertEqual(r["refund"][2], 900)
        self.assertEqual(r["total_pot"], 800 + 150 + 100, "eight antes, the blinds, and the 100 the BB matched")
        self.assertEqual(r["net"][2], 700 + 150, "wins the other seven antes and both blinds")
        self.assertEqual(sum(r["net"]), 0)
        self.assertEqual(r["showdown"], [])

    def test_side_pots(self):
        # A short stack with the best hand wins only the main pot; the next best wins the side pot.
        folded = {3, 4, 5, 6, 7}
        committed = [500, 2000, 2000, 100, 100, 100, 100, 100]
        rank = {0: 3, 1: 2, 2: 1}
        refund, payout, total = settle_pots(committed, folded, rank.__getitem__, 7)
        main = 500 * 3 + 500
        side = 1500 * 2
        self.assertEqual(refund, [0] * 8)
        self.assertEqual(payout, [main, side, 0, 0, 0, 0, 0, 0])
        self.assertEqual(total, main + side)

    def test_three_levels_and_refund(self):
        committed = [300, 800, 1500, 2000]  # top refund 500, levels 300 / 800 / 1500
        refund, payout, total = settle_pots(committed, set(), {0: 9, 1: 5, 2: 3, 3: 1}.__getitem__, 3)
        self.assertEqual(refund, [0, 0, 0, 500])
        self.assertEqual(total, 300 + 800 + 1500 + 1500)
        self.assertEqual(payout, [1200, 500 * 3, 700 * 2, 0])

    def test_odd_chip_goes_left_of_button(self):
        committed = [501, 500, 500]
        refund, payout, _ = settle_pots(committed, {2}, {0: 5, 1: 5}.__getitem__, 2)
        self.assertEqual(refund[0], 1)
        self.assertEqual(payout, [750, 750, 0])
        committed = [500, 500, 1]
        refund, payout, _ = settle_pots(committed, {2}, {0: 5, 1: 5}.__getitem__, 0)
        self.assertEqual(refund, [0, 0, 0])
        self.assertEqual(payout, [500, 501, 0], "seat 1 is first left of button 0")

    def test_refund_measured_against_folded_contribution(self):
        # The second-highest contribution belongs to a seat that folded.
        refund, payout, total = settle_pots([1000, 700, 100], {1, 2}, {}.__getitem__, 0)
        self.assertEqual(refund, [300, 0, 0])
        self.assertEqual((payout, total), ([1500, 0, 0], 1500))

    def test_all_in_called_then_split_with_real_cards(self):
        hole = [["As", "Kd"], ["Ah", "Kc"], ["2d", "7c"], ["3d", "8c"], ["4d", "9c"], ["5d", "Tc"], ["6d", "Jc"], ["7d", "Qc"]]
        h = Hand([20000] * 8, 7, 50, 100, 100, hole, ["Qs", "Jd", "Th", "2s", "3s"])
        h.apply("fold")
        for _ in range(5):
            h.apply("fold")
        h.apply("all_in")  # seat 0
        h.apply("call")
        while h.street != "river":
            h.next_street()
        r = h.settle()
        self.assertEqual(r["winners"], [0, 1])
        self.assertEqual(r["net"][0], 300)
        self.assertEqual(r["net"][1], 300)
        self.assertEqual(sum(r["net"]), 0)


class Dealing(unittest.TestCase):
    NAMES = [f"b{i}" for i in range(8)]

    def test_round_shares_board_and_binds_cards_to_positions(self):
        seen_board = set()
        per_bot = collections.defaultdict(list)
        for hand_no in range(1, 9):
            round_no, button, seating, hole, board = arena.hand_layout(7, hand_no, self.NAMES)
            self.assertEqual(round_no, 0)
            self.assertEqual(button, hand_no - 1)
            seen_board.add(tuple(board))
            for seat, name in enumerate(seating):
                pos = (seat - button - 1) % 8
                per_bot[name].append((pos, tuple(hole[seat])))
        self.assertEqual(len(seen_board), 1)
        sets = {name: sorted(v) for name, v in per_bot.items()}
        first = sets[self.NAMES[0]]
        self.assertEqual(sorted(p for p, _ in first), list(range(8)), "every bot plays every position once")
        for v in sets.values():
            self.assertEqual(v, first, "same position always holds the same cards")
        cards = {c for _, h in first for c in h} | set(seen_board.pop())
        self.assertEqual(len(cards), 21, "no card repeats within the round")

    def test_rounds_differ_and_repeat_for_a_seed(self):
        a = arena.hand_layout(7, 1, self.NAMES)
        b = arena.hand_layout(7, 9, self.NAMES)
        self.assertNotEqual(a[4], b[4])
        self.assertEqual(a, arena.hand_layout(7, 1, self.NAMES))
        self.assertNotEqual(a[4], arena.hand_layout(8, 1, self.NAMES)[4])

    def test_stack_rules(self):
        t = {"big_blind": 100, "start_bb": 200, "rebuy_below_bb": 20, "reset_above_bb": 400}
        self.assertEqual(arena.apply_stack_rules(1999, t), 20000)
        self.assertEqual(arena.apply_stack_rules(2000, t), 2000, "exactly 20BB is not below the line")
        self.assertEqual(arena.apply_stack_rules(40000, t), 40000, "exactly 400BB is not above the line")
        self.assertEqual(arena.apply_stack_rules(40001, t), 20000)
        self.assertEqual(arena.apply_stack_rules(0, t), 20000)
        flat = {**t, "flat_stacks": True}
        for stack in (0, 1999, 12345, 20000, 40001):
            self.assertEqual(arena.apply_stack_rules(stack, flat), 20000)


def read_text(path):
    with open(path, encoding="utf-8") as f:
        return f.read()


def stub_config(path, start_bb=30, rebuy=3, reset=60):
    args = ["--advisor-bin", os.path.join(HERE, "stub_advisor.py"), "--cropped-checkpoint", "stub", "--bucket-table", "stub",
            "--search", "--range-log", "off", "--exploit-4bet", "on"]
    cfg = {"table": {"seats": 8, "small_blind": 50, "big_blind": 100, "ante": 100, "start_bb": start_bb,
                     "rebuy_below_bb": rebuy, "reset_above_bb": reset},
           "groups": [{"name": g, "copies": 2, "args": args} for g in ("alpha", "beta", "gamma", "delta")]}
    with open(path, "w") as f:
        json.dump(cfg, f)


def run_arena(cfg, out, hands, *extra):
    return subprocess.run([sys.executable, os.path.join(HERE, "arena.py"), "--config", cfg, "--out", out, "--hands", str(hands),
                           "--seed", "5", "--mem-every", "1", "--min-avail-gib", "0", "--decision-timeout", "20", *extra],
                          cwd=REPO, capture_output=True, text=True, timeout=600)


class EndToEnd(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        os.chmod(os.path.join(HERE, "stub_advisor.py"), os.stat(os.path.join(HERE, "stub_advisor.py")).st_mode | stat.S_IXUSR)
        cls.tmp = tempfile.mkdtemp(prefix="arena_test_")
        cls.cfg = os.path.join(cls.tmp, "cfg.json")
        stub_config(cls.cfg)
        cls.out = os.path.join(cls.tmp, "run")
        cls.hands_total = 160
        first = run_arena(cls.cfg, cls.out, cls.hands_total, "--stop-after", "60")
        cls.first = first
        second = run_arena(cls.cfg, cls.out, cls.hands_total)
        cls.second = second
        cls.analysis = report.analyse(cls.out)

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.tmp, ignore_errors=True)

    def test_runs_finish(self):
        self.assertEqual(self.first.returncode, 0, self.first.stdout + self.first.stderr)
        self.assertEqual(self.second.returncode, 0, self.second.stdout + self.second.stderr)

    def test_all_hands_recorded_once(self):
        nos = [h["hand_no"] for h in self.analysis["hands"]]
        self.assertEqual(nos, list(range(1, self.hands_total + 1)))

    def test_drivers_accept_every_message(self):
        for name, d in self.analysis["drivers"].items():
            self.assertEqual(d["hh"], self.hands_total, name)
            self.assertFalse(d["kinds"], f"{name}: {d['kinds']}")

    def test_replay_and_conservation_pass(self):
        self.assertFalse(self.analysis["problems"], self.analysis["problems"])

    def test_no_table_corrections(self):
        self.assertFalse(self.analysis["notes"], self.analysis["notes"])
        log = read_text(os.path.join(self.out, "arena.log"))
        self.assertNotIn("[warn]", log)

    def test_stress_covered_the_hard_cases(self):
        kinds = collections.Counter()
        for h in self.analysis["hands"]:
            res = h["result"]
            if len(res["payouts"]) > 1:
                kinds["multi_payout"] += 1
            final = {int(k): v for k, v in res["final_stacks"].items()}
            if h["stack_resets"]:
                kinds["stack_reset"] += 1
            for a in h["actions"]:
                if a["action"] == "call" and a["contribution_delta"] < a["to_call_before"]:
                    kinds["call_for_less"] += 1
                if a["action"] == "all_in" and a["to"] and a["to_call_before"] >= 0:
                    kinds["all_in_raise"] += 1
            self.assertEqual(sum(final.values()), sum(h["initial_stacks"].values()))
        for k in ("multi_payout", "stack_reset", "call_for_less", "all_in_raise"):
            self.assertGreater(kinds[k], 0, f"{k} not exercised: {kinds}")

    def test_stacks_carry_over_with_rules(self):
        t = self.analysis["run"]["config"]["table"]
        prev = None
        for h in self.analysis["hands"]:
            names = {int(s): n for s, n in h["names"].items()}
            start = {names[int(s)]: v for s, v in h["initial_stacks"].items()}
            if prev is not None:
                for name, v in start.items():
                    self.assertEqual(v, arena.apply_stack_rules(prev[name], t), f"{h['hand_id']} {name}")
            prev = {names[int(s)]: v for s, v in h["result"]["final_stacks"].items()}

    def test_duplicate_deal_visible_in_table_record(self):
        rounds = collections.defaultdict(list)
        for h in self.analysis["hands"]:
            rounds[h["round_no"]].append(h)
        self.assertEqual(len(rounds), self.hands_total // 8)
        for rs in rounds.values():
            self.assertEqual(len(rs), 8)
            by_pos = {}
            for h in rs:
                names = {int(s): n for s, n in h["names"].items()}
                self.assertEqual(names, {int(s): n for s, n in rs[0]["names"].items()}, "seating fixed within a round")
                shown = {int(k): v for k, v in h["result"]["shown_cards"].items()}
                shown[0] = h["hole"]
                self.assertEqual(len(shown), 8)
                for s, cards in shown.items():
                    pos = (s - h["button_seat"] - 1) % 8
                    self.assertEqual(by_pos.setdefault(pos, cards), cards)

    def test_drivers_only_see_own_and_showdown_cards(self):
        name = self.analysis["hands"][0]["names"]["0"]
        for line in read_text(os.path.join(self.out, name, "wopo_hh.jsonl")).splitlines():
            r = json.loads(line)
            self.assertEqual(r["my_seat"], 0)
            shown = r["result"].get("shown_cards") or {}
            self.assertNotIn("0", shown)
            folded = {a["seat"] for a in r["actions"] if a["action"] == "fold"}
            self.assertFalse(folded & {int(k) for k in shown}, "folded cards must stay hidden from drivers")

    def test_table_record_builds_replay_with_decisions(self):
        events = collections.defaultdict(list)
        for line in read_text(os.path.join(self.out, "table", "wopo_play_events.jsonl")).splitlines():
            e = json.loads(line)
            events[e["hand_id"]].append(e)
        for h in self.analysis["hands"][:40]:
            view = build_hand(h, events[h["hand_id"]], None, "table", "x")
            self.assertIsNone(view["check"], view["check"])
            self.assertEqual(view["notes"], [])
            steps = [s for s in view["steps"] if s["kind"] == "action"]
            self.assertTrue(steps)
            for s in steps:
                self.assertIsNotNone(s.get("hero"), "every seat's decision is shown")
                self.assertEqual(s["hero"]["match"], "same")
                self.assertEqual(s["hero"]["decision"]["source"], "stub")

    def test_report_renders(self):
        text = report.format_report(self.analysis)
        for g in ("alpha", "beta", "gamma", "delta"):
            self.assertIn(g, text)
        self.assertIn("全部通过", text)


class NextStage(unittest.TestCase):
    """A later stage: new directory, hand numbers and deals carry on, profiles warm up, every hand at the start stack."""

    def test_flat_stacks_stage(self):
        tmp = tempfile.mkdtemp(prefix="arena_stage_")
        try:
            cfg = os.path.join(tmp, "cfg.json")
            stub_config(cfg)
            first, second = os.path.join(tmp, "first"), os.path.join(tmp, "second")
            r = run_arena(cfg, first, 16)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            r = run_arena(cfg, second, 48, "--flat-stacks", "--first-hand", "17", "--warm-from", first)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            a = report.analyse(second)
            self.assertEqual([h["hand_no"] for h in a["hands"]], list(range(17, 49)))
            self.assertFalse(a["problems"], a["problems"])
            for name, d in a["drivers"].items():
                self.assertEqual(d["hh"], 32, name)
                self.assertFalse(d["kinds"], f"{name}: {d['kinds']}")
                self.assertIn("16 手发给 advisor", read_text(os.path.join(second, name, "driver.log")))
            for h in a["hands"]:
                self.assertEqual(set(h["initial_stacks"].values()), {30 * 100})
                self.assertEqual(h["stack_resets"], {})
                self.assertEqual(sum(h["net_by_bot"].values()), 0)
            boards_first = {tuple(h["board"]) for h in report.analyse(first)["hands"] if len(h["board"]) == 5}
            boards_second = {tuple(h["board"]) for h in a["hands"] if len(h["board"]) == 5}
            self.assertFalse(boards_first & boards_second, "a new stage deals new cards")
        finally:
            shutil.rmtree(tmp, ignore_errors=True)


class LongWarmStart(unittest.TestCase):
    """A warm start sends thousands of observes before the first decide. Acks are not read until then, so
    without a periodic drain both sides of the advisor's pipes fill up and block for good (2026-11-06: 2000
    hands hung every advisor when the arena resumed)."""

    def test_many_observes_then_decide(self):
        from poker_live.advisor_client import Advisor
        os.chmod(os.path.join(HERE, "stub_advisor.py"), os.stat(os.path.join(HERE, "stub_advisor.py")).st_mode | stat.S_IXUSR)
        adv = Advisor(os.path.join(HERE, "stub_advisor.py"), "x", "y", "r", 4, 1, extra_args=["--seats", "8"], cropped=True, n_seats=8)
        out = {}
        observe = {"observe": 1, "names": {str(i): "name" * 20 for i in range(8)},
                   "actions": [{"seat": i % 8, "street": "preflop", "action": "fold"} for i in range(40)]}
        request = {"valid": {"can_check": True, "can_call": False, "can_raise": False, "min_raise": None, "max_raise": None}}

        def work():
            for _ in range(5000):
                adv.observe(observe)
            out["resp"] = adv.decide(request)

        t = threading.Thread(target=work, daemon=True)
        t.start()
        t.join(60)
        hung = t.is_alive()
        adv.proc.kill()
        adv.proc.wait()
        self.assertFalse(hung, "advisor pipes deadlocked")
        self.assertEqual(out["resp"]["source"], "stub", "the decide response must not be mistaken for an ack")


class SignalHandling(unittest.TestCase):
    def test_sigterm_stops_at_hand_boundary_and_resumes(self):
        tmp = tempfile.mkdtemp(prefix="arena_sig_")
        try:
            cfg = os.path.join(tmp, "cfg.json")
            stub_config(cfg)
            out = os.path.join(tmp, "run")
            p = subprocess.Popen([sys.executable, os.path.join(HERE, "arena.py"), "--config", cfg, "--out", out, "--hands", "8000",
                                  "--seed", "9", "--mem-every", "1", "--min-avail-gib", "0"], cwd=REPO, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            hh = os.path.join(out, "table", "wopo_hh.jsonl")
            deadline = time.monotonic() + 120
            while time.monotonic() < deadline and not (os.path.exists(hh) and os.path.getsize(hh) > 0):
                time.sleep(0.05)
            p.send_signal(signal.SIGTERM)
            self.assertEqual(p.wait(timeout=120), 3)
            lines = read_text(hh).splitlines()
            self.assertTrue(lines)
            last = json.loads(lines[-1])
            r = run_arena(cfg, out, last["hand_no"] + 16 - (last["hand_no"] + 16) % 8, "--seed", "9")
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            nos = [json.loads(x)["hand_no"] for x in read_text(hh).splitlines()]
            self.assertEqual(nos, list(range(1, nos[-1] + 1)))
            deadline = time.monotonic() + 15
            leftover = None
            while time.monotonic() < deadline:
                leftover = subprocess.run(["pgrep", "-f", out], capture_output=True, text=True).stdout.strip()
                if not leftover:
                    break
                time.sleep(0.2)
            self.assertFalse(leftover, "drivers and advisors must be gone")
        finally:
            shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    unittest.main()

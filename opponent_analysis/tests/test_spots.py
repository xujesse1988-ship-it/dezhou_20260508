import unittest

from opponent_analysis.adapters import poker_manager_raw
from opponent_analysis.replay import replay
from opponent_analysis.spots import hand_observations
from opponent_analysis.tests.helpers import pm_record


def obs(rec, pct=None):
    h = replay(poker_manager_raw(rec, "test"), heads_up_button_is_sb=True)
    return [(o.name, o.stat, o.outcome) for o in hand_observations(h, pct)], hand_observations(h, pct)


class Preflop(unittest.TestCase):
    def test_open_3bet_fold(self):
        rec = pm_record("h1", 0, [0, 1, 2, 3], {s: 2000 for s in range(4)},
                        [(3, "raise", 60), (0, "fold", None), (1, "raise", 230), (2, "fold", None), (3, "fold", None)],
                        {0: 2000, 1: 2080, 2: 1980, 3: 1940})
        got, _ = obs(rec)
        self.assertIn(("p3", "rfi", "raise"), got)
        self.assertIn(("p3", "vs_3bet", "fold"), got)
        self.assertIn(("p1", "vs_open", "raise"), got)
        self.assertIn(("p1", "steal_defense", "raise"), got)  # opener seat 3 is CO 4-handed
        self.assertIn(("p0", "vs_open", "fold"), got)
        self.assertIn(("p2", "vpip", "no"), got)
        self.assertNotIn("rfi", {s for n, s, _ in got if n == "p2"})  # BB never faces an unopened pot here
        self.assertEqual(sorted(o for n, s, o in got if s == "vpip"), ["no", "no", "yes", "yes"])

    def test_bb_walk_has_no_vpip_opportunity(self):
        rec = pm_record("h2", 0, [0, 1, 2], {s: 1000 for s in range(3)},
                        [(0, "fold", None), (1, "fold", None)], {0: 1000, 1: 990, 2: 1010})
        got, _ = obs(rec)
        self.assertFalse([1 for n, s, _ in got if n == "p2" and s == "vpip"])


class Postflop(unittest.TestCase):
    def test_cbet_fold_with_pot_and_bet(self):
        rec = pm_record("h3", 0, [0, 1, 2], {s: 1000 for s in range(3)},
                        [(0, "raise", 60), (1, "fold", None), (2, "call", 40),
                         (2, "check", None), (0, "raise", 50), (2, "fold", None)],
                        {0: 1070, 1: 990, 2: 940}, board=["2c", "3d", "4h"])
        got, full = obs(rec)
        self.assertIn(("p0", "cbet", "bet"), got)
        self.assertIn(("p2", "vs_cbet", "fold"), got)
        vs = [o for o in full if o.stat == "vs_bet_flop"][0]
        self.assertEqual((vs.pot_bb, vs.bet_bb), (130 / 20, 50 / 20))
        self.assertEqual(vs.dims["after_check"], "yes")

    def test_clean_river_sample(self):
        rec = pm_record("h4", 0, [0, 1, 2], {s: 1000 for s in range(3)},
                        [(0, "raise", 60), (1, "fold", None), (2, "call", 40),
                         (2, "check", None), (0, "check", None), (2, "check", None), (0, "check", None),
                         (2, "check", None), (0, "raise", 100), (2, "call", 100)],
                        {0: 1240, 1: 990, 2: 770}, board=["2c", "3d", "4h", "8s", "9c"],
                        shown={0: ["7h", "6d"], 2: ["Kh", "Kd"]})
        got, full = obs(rec, pct=lambda hole, board: 0.1)
        rb = [o for o in full if o.stat == "river_bet_called"]
        self.assertEqual(len(rb), 1)
        self.assertEqual((rb[0].name, rb[0].pot_bb, rb[0].bet_bb, rb[0].pct), ("p0", 130 / 20, 5.0, 0.1))
        self.assertIn(("p2", "wtsd", "yes"), got)
        self.assertIn(("p0", "wsd", "won"), got)
        self.assertIn(("p2", "wsd", "lost"), got)

    def test_uncalled_return_is_not_a_showdown_win(self):
        # BTN shoves 2000 into a 500 BB stack and loses; the 1500 nobody matched comes back.
        rec = pm_record("h6", 0, [0, 1, 2], {0: 2000, 1: 1000, 2: 500},
                        [(0, "all_in", 2000), (1, "fold", None), (2, "call", 480)],
                        {0: 1500, 1: 990, 2: 1010}, board=["2c", "3d", "4h", "8s", "9c"],
                        shown={0: ["7h", "6d"], 2: ["Kh", "Kd"]})
        got, _ = obs(rec)
        self.assertIn(("p0", "wsd", "lost"), got)
        self.assertIn(("p2", "wsd", "won"), got)

    def test_bet_above_callers_stack_counts_as_his_stack(self):
        # Flop: BTN bets 500 into 130, BB has 340 left and folds -> only 340 was at stake.
        rec = pm_record("h7", 0, [0, 1, 2], {0: 2000, 1: 1000, 2: 400},
                        [(0, "raise", 60), (1, "fold", None), (2, "call", 40), (2, "check", None), (0, "bet", 500),
                         (2, "fold", None)],
                        {0: 2070, 1: 990, 2: 340}, board=["2c", "7d", "Kh"])
        _, full = obs(rec)
        vs = [o for o in full if o.stat == "vs_bet_flop"][0]
        self.assertEqual((vs.pot_bb, vs.bet_bb), (130 / 20, 340 / 20))

    def test_river_short_call_uses_the_matched_amount(self):
        # River: BTN bets 500, BB calls all-in for 240 and the other 260 goes back.
        rec = pm_record("h8", 0, [0, 1, 2], {0: 2000, 1: 1000, 2: 300},
                        [(0, "raise", 60), (1, "fold", None), (2, "call", 40)]
                        + [(2, "check", None), (0, "check", None)] * 2 + [(2, "check", None), (0, "bet", 500),
                                                                          (2, "call", 240)],
                        {0: 1700, 1: 990, 2: 610}, board=["2c", "3d", "4h", "8s", "9c"],
                        shown={0: ["7h", "6d"], 2: ["Kh", "Kd"]})
        _, full = obs(rec, pct=lambda hole, board: 0.1)
        rb = [o for o in full if o.stat == "river_bet_called"][0]
        self.assertEqual((rb.pot_bb, rb.bet_bb), (130 / 20, 240 / 20))

    def test_forced_decisions_are_skipped(self):
        rec = pm_record("h5", 0, [0, 1, 2], {s: 1000 for s in range(3)},
                        [(0, "fold", None), (1, "fold", None)], {0: 1000, 1: 990, 2: 1010})
        rec["result"]["actions"][0]["reason"] = "timeout"
        got, _ = obs(rec)
        self.assertFalse([1 for n, _, _ in got if n == "p0"])


if __name__ == "__main__":
    unittest.main()

import unittest

from opponent_analysis.adapters import openpoker_raw, poker_manager_raw
from opponent_analysis.replay import ReplayError, assign_positions, replay
from opponent_analysis.tests.helpers import pm_record


def run_pm(rec):
    return replay(poker_manager_raw(rec, "test"), heads_up_button_is_sb=True)


class Positions(unittest.TestCase):
    def test_six_handed_labels(self):
        pos, sb, bb = assign_positions([0, 1, 2, 3, 4, 5], 2, True)
        self.assertEqual((sb, bb), (3, 4))
        self.assertEqual(pos, {2: "BTN", 3: "SB", 4: "BB", 5: "UTG", 0: "HJ", 1: "CO"})

    def test_short_table_skips_empty_seats(self):
        pos, sb, bb = assign_positions([1, 4, 7], 7, True)
        self.assertEqual((sb, bb), (1, 4))
        self.assertEqual(pos[7], "BTN")
        pos, _, _ = assign_positions(list(range(9)), 0, True)
        self.assertEqual([pos[s] for s in range(3, 9)], ["UTG", "UTG1", "UTG2", "LJ", "HJ", "CO"])

    def test_heads_up_rule_is_per_platform(self):
        _, sb, bb = assign_positions([2, 5], 2, True)
        self.assertEqual((sb, bb), (2, 5))
        _, sb, bb = assign_positions([2, 5], 2, False)
        self.assertEqual((sb, bb), (5, 2))


class Replay(unittest.TestCase):
    def test_open_3bet_fold(self):
        # 4-handed, button 0: SB 1, BB 2, UTG(=CO) 3 acts first.
        rec = pm_record("h1", 0, [0, 1, 2, 3], {s: 2000 for s in range(4)},
                        [(3, "raise", 60), (0, "fold", None), (1, "raise", 230), (2, "fold", None), (3, "fold", None)],
                        {0: 2000, 1: 2080, 2: 1980, 3: 1940})
        h = run_pm(rec)
        kinds = [(d.seat, d.kind, d.raises_before, d.to_call) for d in h.decisions]
        self.assertEqual(kinds, [(3, "raise", 0, 20), (0, "fold", 1, 60), (1, "raise", 1, 50),
                                 (2, "fold", 2, 220), (3, "fold", 2, 180)])
        self.assertEqual(h.preflop_aggressor, 1)
        self.assertEqual(h.showdown_seats, [])
        self.assertEqual(h.committed, {0: 0, 1: 240, 2: 20, 3: 60})

    def test_streets_and_showdown(self):
        rec = pm_record("h2", 0, [0, 1, 2], {s: 1000 for s in range(3)},
                        [(0, "raise", 60), (1, "fold", None), (2, "call", 40),
                         (2, "check", None), (0, "raise", 50), (2, "call", 50),
                         (2, "check", None), (0, "check", None),
                         (2, "check", None), (0, "check", None)],
                        {0: 1240, 1: 990, 2: 890 - 120}, board=["2c", "3d", "4h", "5s", "9c"],
                        shown={0: ["Ah", "Ad"], 2: ["Kh", "Kd"]})
        # fix final stacks to keep the table zero-sum: 0 wins 10+110+110
        rec["result"]["final_stacks"] = {"0": 1120, "1": 990, "2": 890}
        h = run_pm(rec)
        self.assertEqual([d.street for d in h.decisions],
                         ["preflop"] * 3 + ["flop"] * 3 + ["turn"] * 2 + ["river"] * 2)
        flop_bet = h.decisions[4]
        self.assertEqual((flop_bet.kind, flop_bet.pot_before, flop_bet.street_start_pot), ("bet", 130, 130))
        self.assertEqual(h.showdown_seats, [0, 2])

    def test_missing_action_is_rejected(self):
        # BB's check option is missing: the next street cannot start.
        rec = pm_record("h3", 0, [0, 1, 2], {s: 1000 for s in range(3)},
                        [(0, "call", 20), (1, "call", 10), (1, "check", None)],
                        {0: 980, 1: 980, 2: 980})
        with self.assertRaises(ReplayError) as cm:
            run_pm(rec)
        self.assertEqual(cm.exception.code, "out_of_turn")

    def test_not_zero_sum_is_rejected(self):
        rec = pm_record("h4", 0, [0, 1, 2], {s: 1000 for s in range(3)},
                        [(0, "fold", None), (1, "fold", None)], {0: 1000, 1: 990, 2: 1015})
        with self.assertRaises(ReplayError) as cm:
            run_pm(rec)
        self.assertEqual(cm.exception.code, "not_zero_sum")

    def test_short_all_in_call(self):
        rec = pm_record("h5", 0, [0, 1, 2], {0: 1000, 1: 1000, 2: 50},
                        [(0, "raise", 200), (1, "fold", None), (2, "all_in", 30)],
                        {0: 1060, 1: 990, 2: 0}, board=["2c", "3d", "4h", "5s", "9c"])
        h = run_pm(rec)
        last = h.decisions[-1]
        self.assertEqual((last.kind, last.all_in, last.delta), ("call", True, 30))
        self.assertEqual(h.showdown_seats, [0, 2])

    def test_openpoker_all_in_without_amount(self):
        rec = {"hh": 1, "hand_id": "o1", "table_id": "t", "button_seat": 0, "my_seat": 0, "small_blind": 10,
               "big_blind": 20, "board": ["2c", "3d", "4h", "5s", "9c"], "names": {"0": "a", "1": "b", "2": "c"},
               "actions": [{"seat": 0, "action": "all_in"}, {"seat": 1, "action": "fold"}, {"seat": 2, "action": "call"}],
               "actions_ext": [{"stack_before": 500, "contribution_delta": 0}, {"stack_before": 990, "contribution_delta": 0},
                               {"stack_before": 980, "contribution_delta": 480}],
               "hand_result": {"final_stacks": {"0": 1010, "1": 990, "2": 500}, "winners": [{"seat": 0, "amount": 510}],
                               "shown_cards": {"0": ["Ah", "Ad"], "2": ["Kh", "Kd"]}}}
        h = replay(openpoker_raw(rec, "test"), heads_up_button_is_sb=False)
        self.assertEqual(h.decisions[0].delta, 500)
        self.assertEqual(h.start_stacks, {0: 500, 1: 1000, 2: 1000})


if __name__ == "__main__":
    unittest.main()

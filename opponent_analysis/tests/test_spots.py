import unittest

from opponent_analysis.adapters import poker_manager_raw
from opponent_analysis.replay import replay
from opponent_analysis.spots import hand_observations, preflop_allins
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

    def test_limp_spots(self):
        # 6-handed, button 0: p3 limps, p4 iso-raises, p5 / p0 / p1 fold, p2 (BB) calls, p3 folds; checked down.
        rec = pm_record("h5", 0, list(range(6)), {s: 2000 for s in range(6)},
                        [(3, "call", 20), (4, "raise", 100), (5, "fold", None), (0, "fold", None),
                         (1, "fold", None), (2, "call", 80), (3, "fold", None),
                         (2, "check", None), (4, "check", None), (2, "check", None), (4, "check", None),
                         (2, "check", None), (4, "check", None)],
                        {0: 2000, 1: 1990, 2: 1900, 3: 1980, 4: 2130, 5: 2000}, board=["2c", "3d", "4h", "8s", "9c"])
        got, _ = obs(rec)
        self.assertIn(("p4", "vs_limp", "raise"), got)
        self.assertIn(("p3", "limp_vs_raise", "fold"), got)
        self.assertIn(("p2", "vs_open", "call"), got)
        self.assertIn(("p5", "vs_open", "fold"), got)
        self.assertNotIn("vs_limp", {s for n, s, _ in got if n == "p3"})
        self.assertNotIn("limp_vs_raise", {s for n, s, _ in got if n != "p3"})
        # 4-handed: SB completes, BB checks with one caller in: blind option, not multi.
        rec = pm_record("h6", 0, list(range(4)), {s: 2000 for s in range(4)},
                        [(3, "fold", None), (0, "fold", None), (1, "call", 10), (2, "check", None),
                         (1, "check", None), (2, "check", None), (1, "check", None), (2, "check", None),
                         (1, "check", None), (2, "check", None)],
                        {0: 2000, 1: 2020, 2: 1980, 3: 2000}, board=["2c", "3d", "4h", "8s", "9c"])
        got, full = obs(rec)
        bo = [(o.name, o.outcome, o.dims["bb"], o.dims["multi"]) for o in full if o.stat == "blind_option"]
        self.assertEqual(bo, [("p2", "check", "yes", "no")])
        self.assertNotIn("vs_limp", {s for n, s, _ in got})  # SB completes into an unlimped pot: rfi

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


class PreflopAllins(unittest.TestCase):
    def allins(self, rec):
        h = replay(poker_manager_raw(rec, "test"), heads_up_button_is_sb=True)
        return h, preflop_allins(h)

    def test_jam_and_call_with_shown_cards(self):
        # CO opens, BTN jams over it, blinds fold, CO calls all-in; both show.
        rec = pm_record("a1", 0, [0, 1, 2, 3], {0: 2000, 1: 2000, 2: 2000, 3: 1000},
                        [(3, "raise", 60), (0, "all_in", 2000), (1, "fold", None), (2, "fold", None),
                         (3, "call", 940)],
                        {0: 3030, 1: 1990, 2: 1980, 3: 0}, board=["2c", "3d", "4h", "8s", "9c"],
                        shown={0: ["Ah", "5h"], 3: ["Kd", "Kc"]})
        _, got = self.allins(rec)
        self.assertEqual([n for n, _ in got], ["p0", "p3"])
        jam, call = got[0][1], got[1][1]
        self.assertEqual((jam.action, jam.facing, jam.pos, jam.combo, jam.n_live), ("jam", 1, "BTN", "A5s", 4))
        self.assertEqual((jam.stack_bb, jam.eff_bb), (100.0, 100.0))
        # CO faces open + jam = a 3bet; his 1000 is the effective stack.
        self.assertEqual((call.action, call.facing, call.combo, call.cards), ("call", 2, "KK", ["Kd", "Kc"]))
        self.assertEqual((call.stack_bb, call.eff_bb, call.n_live), (50.0, 50.0, 2))

    def test_uncalled_jam_has_no_cards_and_folded_seats_do_not_count(self):
        # Two fold, BTN (3000) jams into SB 400 / BB 600: effective = the biggest live other = 600.
        rec = pm_record("a2", 0, [0, 1, 2, 3, 4], {0: 3000, 1: 400, 2: 600, 3: 5000, 4: 5000},
                        [(3, "fold", None), (4, "fold", None), (0, "all_in", 3000), (1, "fold", None),
                         (2, "fold", None)],
                        {0: 3030, 1: 390, 2: 580, 3: 5000, 4: 5000})
        _, got = self.allins(rec)
        self.assertEqual(len(got), 1)
        a = got[0][1]
        self.assertEqual((a.action, a.facing, a.callers, a.combo, a.cards), ("jam", 0, 0, None, None))
        self.assertEqual((a.stack_bb, a.eff_bb), (150.0, 30.0))

    def test_summary_groups_and_report_section(self):
        from opponent_analysis.report import Accumulator, Config, allin_summary, render_markdown

        acc = Accumulator("poker_manager", pct_fn=lambda hole, board: 0.5)
        for i, (hole, open_first) in enumerate([(["Ah", "5h"], True), (["As", "Ad"], True), (["Ah", "5h"], True),
                                                (None, False)]):
            acts = ([(3, "raise", 60)] if open_first else [(3, "fold", None)]) + [(0, "all_in", 2000), (1, "fold", None),
                                                                                  (2, "fold", None)]
            acts += [(3, "call", 1940)] if open_first else []
            final = {0: 4030, 1: 1990, 2: 1980, 3: 0} if open_first else {0: 2030, 1: 1990, 2: 1980, 3: 2000}
            rec = pm_record(f"s{i}", 0, [0, 1, 2, 3], {s: 2000 for s in range(4)}, acts, final,
                            board=["2c", "3d", "4h", "8s", "9c"] if open_first else (),
                            shown={0: hole, 3: ["7c", "2d"]} if hole else None, hero=1)
            acc.add(replay(poker_manager_raw(rec, "test"), heads_up_button_is_sb=True))
        sm = allin_summary(acc.players["p0"])
        self.assertEqual((sm["n"], sm["jam"], sm["call"], sm["n_shown"]), (4, 4, 0, 3))
        self.assertEqual([(g["label"], g["n"], g["n_shown"], g["combos"]) for g in sm["groups"]],
                         [("推全下·未开池", 1, 0, []), ("推全下·面对开池", 3, 3, [("AA", 1), ("A5s", 2)])])
        self.assertEqual(sm["shown"][0].hand_id, "s2")  # latest first
        md = render_markdown(acc.finalize([], Config()))
        self.assertIn("推全下·面对开池：3 次，亮 3 次：AA、A5s×2", md)
        self.assertIn("跟全下·面对 3bet：3 次，亮 3 次：72o×3", md)

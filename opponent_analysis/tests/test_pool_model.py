import json
import os
import tempfile
import unittest

from opponent_analysis import pool_model as pm
from opponent_analysis.adapters import poker_manager_raw
from opponent_analysis.replay import replay
from opponent_analysis.tests.helpers import pm_record


def hand(rec):
    return replay(poker_manager_raw(rec, "test"), heads_up_button_is_sb=True)


def rows(h, heroes=()):
    return [(o.name, o.family, o.outcome) for o in pm.hand_obs(h, heroes)]


# 6-handed, button 0 (SB 1, BB 2), blinds 10 / 20. p3 limps, p4 raises to 100, p5 / p0 / p1 fold,
# p2 calls, p3 folds. Flop (pot 230): p2 checks, p4 bets 115, p2 calls. Turn: check, check.
# River (pot 460): p2 bets 230, p4 raises to 690, p2 folds.
LIMP_HAND = pm_record(
    "limp", 0, list(range(6)), {s: 5000 for s in range(6)},
    [(3, "call", 20), (4, "raise", 100), (5, "fold", None), (0, "fold", None), (1, "fold", None),
     (2, "call", 80), (3, "fold", None),
     (2, "check", None), (4, "bet", 115), (2, "call", 115),
     (2, "check", None), (4, "check", None),
     (2, "bet", 230), (4, "raise", 690), (2, "fold", None)],
    {0: 5000, 1: 4990, 2: 4555, 3: 4980, 4: 5475, 5: 5000}, board=["2c", "3d", "4h", "8s", "9c"], hero=0)

# 6-handed, button 0. p3 opens to 60, p4 3bets to 200, p5 / p0 / p1 / p2 fold, p3 4bets to 500, p4 calls;
# checked down.
FOURBET_HAND = pm_record(
    "4bet", 0, list(range(6)), {s: 5000 for s in range(6)},
    [(3, "raise", 60), (4, "raise", 200), (5, "fold", None), (0, "fold", None), (1, "fold", None),
     (2, "fold", None), (3, "raise", 440), (4, "call", 300),
     (3, "check", None), (4, "check", None), (3, "check", None), (4, "check", None),
     (3, "check", None), (4, "check", None)],
    {0: 5000, 1: 4990, 2: 4980, 3: 5530, 4: 4500, 5: 5000}, board=["2c", "3d", "4h", "8s", "9c"],
    shown={3: ["As", "Ad"], 4: ["Kc", "Kd"]}, hero=0)


class Sizes(unittest.TestCase):
    def test_raise_fraction_is_the_tree_unit(self):
        # 8 seats, 1 BB ante, blinds 50 / 100: pot 950 before the first action.
        self.assertAlmostEqual(pm.raise_fraction(450, 100, 950, 100), 1 / 3)
        self.assertAlmostEqual(pm.raise_fraction(1150, 100, 950, 100), 1.0)
        # Postflop bet of half the pot.
        self.assertAlmostEqual(pm.raise_fraction(115, 0, 230, 0), 0.5)

    def test_buckets(self):
        self.assertEqual([pm.size_bucket(x) for x in (0.33, 0.42, 0.5, 0.75, 1.0, 1.5, 3.0)],
                         ["<=0.42", "<=0.42", "0.42-0.62", "0.62-0.87", "0.87-1.2", "1.2-2.0", ">2.0"])


class Families(unittest.TestCase):
    def test_limped_pot(self):
        obs = pm.hand_obs(hand(LIMP_HAND))
        by = {(o.name, o.family): o for o in obs}
        # p0 is the hero seat: none of its decisions are pool observations.
        self.assertFalse([o for o in obs if o.name == "p0"])

        limp = by[("p3", "pf_unopened")]
        self.assertEqual((limp.outcome, limp.dims["pos"]), ("call", "EP"))
        iso = by[("p4", "pf_vs_limp")]
        self.assertEqual((iso.outcome, iso.dims["limpers"]), ("raise", "1"))
        # Raise to 100 over a 20 limp: 80 above the call, pot 50 + the 20 call.
        self.assertAlmostEqual(iso.size, 80 / 70)
        for name, seat in (("p5", "field"), ("p1", "SB"), ("p2", "BB")):
            o = by[(name, "pf_vs_raise")]
            self.assertEqual((o.dims["seat"], o.dims["size"], o.dims["raise_kind"]), (seat, "0.87-1.2", "iso"))
        self.assertEqual(by[("p2", "pf_vs_raise")].outcome, "call")
        back = by[("p3", "pf_limper_vs_raise")]
        self.assertEqual((back.outcome, back.dims["size"], back.dims["callers"]), ("fold", "0.87-1.2", "1+"))

        post = [(o.name, o.family, o.outcome, o.dims) for o in obs if o.family.startswith("po_")]
        self.assertEqual([(n, f, a) for n, f, a, _ in post], [
            ("p2", "po_lead", "check"), ("p4", "po_lead", "bet"), ("p2", "po_vs_bet", "call"),
            ("p2", "po_lead", "check"), ("p4", "po_lead", "check"),
            ("p2", "po_lead", "bet"), ("p4", "po_vs_bet", "raise"), ("p2", "po_vs_raise", "fold")])
        # Flop: p4 raised preflop, acts last heads-up.
        self.assertEqual(post[0][3], {"street": "flop", "role": "other", "players": "2", "seat": "early"})
        self.assertEqual(post[1][3], {"street": "flop", "role": "aggr", "players": "2", "seat": "last"})
        self.assertEqual(post[2][3], {"size": "0.42-0.62", "street": "flop", "players": "2"})
        # Turn: p4 bet the flop. River: nobody bet the turn.
        self.assertEqual(post[4][3]["role"], "aggr")
        self.assertEqual(post[5][3]["role"], "none")
        self.assertEqual(post[6][3]["size"], "0.42-0.62")  # 230 into 460
        self.assertEqual(post[7][3], {"street": "river", "was_bettor": "yes"})

    def test_reraised_pot_and_shown_cards(self):
        obs = pm.hand_obs(hand(FOURBET_HAND))
        got = [(o.name, o.family, o.outcome) for o in obs if o.family.startswith("pf_")]
        self.assertEqual(got, [
            ("p3", "pf_unopened", "raise"), ("p4", "pf_vs_raise", "raise"),
            ("p5", "pf_other_vs_3bet", "fold"), ("p1", "pf_other_vs_3bet", "fold"),
            ("p2", "pf_other_vs_3bet", "fold"),
            ("p3", "pf_raiser_vs_3bet", "raise"), ("p4", "pf_vs_4bet", "call")])
        by = {(o.name, o.family): o for o in obs}
        self.assertEqual(by[("p5", "pf_other_vs_3bet")].dims, {"entered": "no"})
        self.assertEqual(by[("p4", "pf_vs_4bet")].dims, {"raised": "yes"})
        # 3bet to 200 over an open to 60: 140 above the call, pot 90 + the 60 call.
        self.assertEqual(by[("p3", "pf_raiser_vs_3bet")].dims["size"], pm.size_bucket(140 / 150))
        self.assertEqual(by[("p3", "pf_unopened")].combo, "AA")
        self.assertEqual(by[("p4", "pf_vs_4bet")].combo, "KK")
        m = pm.fit(obs)
        self.assertEqual(m["shown"]["pf_raiser_vs_3bet|raise"], {"AA": 1})
        self.assertFalse([k for k in m["shown"] if k.startswith("po_")])

    def test_hero_names_are_left_out(self):
        h = hand(LIMP_HAND)
        self.assertFalse([r for r in rows(h, ["p4"]) if r[0] == "p4"])
        self.assertTrue([r for r in rows(h) if r[0] == "p4"])

    def test_every_family_cell_uses_recorded_dims(self):
        obs = pm.hand_obs(hand(LIMP_HAND)) + pm.hand_obs(hand(FOURBET_HAND))
        for o in obs:
            for d in pm.FAMILIES[o.family] + pm.SIZE_DIMS[o.family]:
                self.assertIn(d, o.dims, (o.family, d))


def _obs(family, dims, outcome, n, name="x", hand_id="h", size=None, all_in=False):
    return [pm.PoolObs(f"{hand_id}{i}", float(i), name, family, dims, outcome, size, all_in) for i in range(n)]


class FitPredict(unittest.TestCase):
    def setUp(self):
        small = {"seat": "BB", "size": "<=0.42"}
        big = {"seat": "BB", "size": "0.87-1.2"}
        sb = {"seat": "SB", "size": "0.87-1.2"}
        self.obs = (_obs("pf_vs_raise", small, "fold", 2) + _obs("pf_vs_raise", small, "call", 6)
                    + _obs("pf_vs_raise", big, "fold", 9) + _obs("pf_vs_raise", big, "raise", 3, size=1.0)
                    + _obs("pf_vs_raise", sb, "fold", 20))
        self.m = pm.fit(self.obs, k=10.0)

    def test_backoff_arithmetic(self):
        p = pm.predict(self.m, "pf_vs_raise", {"seat": "BB", "size": "<=0.42"})
        root = {"fold": 31 / 40, "call": 6 / 40, "raise": 3 / 40}
        bb = {"fold": (11 + 10 * root["fold"]) / 30, "call": (6 + 10 * root["call"]) / 30,
              "raise": (3 + 10 * root["raise"]) / 30}
        want = {"fold": (2 + 10 * bb["fold"]) / 18, "call": (6 + 10 * bb["call"]) / 18, "raise": 10 * bb["raise"] / 18}
        self.assertEqual(p["n"], 8)
        for a, v in want.items():
            self.assertAlmostEqual(p["probs"][a], v)
        self.assertAlmostEqual(sum(p["probs"].values()), 1.0)

    def test_depth_and_unseen_cell(self):
        root = pm.predict(self.m, "pf_vs_raise", {"seat": "BB", "size": "<=0.42"}, depth=0)
        self.assertAlmostEqual(root["probs"]["fold"], 31 / 40)
        self.assertEqual(root["n"], 40)
        # A size nobody was seen facing in this seat: the answer is the seat's own frequencies, n = 0.
        unseen = pm.predict(self.m, "pf_vs_raise", {"seat": "SB", "size": ">2.0"})
        seat = pm.predict(self.m, "pf_vs_raise", {"seat": "SB", "size": ">2.0"}, depth=1)
        self.assertEqual(unseen["n"], 0)
        self.assertEqual(unseen["probs"], seat["probs"])
        # k -> 0 gives the raw cell.
        raw = pm.predict(self.m, "pf_vs_raise", {"seat": "BB", "size": "0.87-1.2"}, k=1e-12)
        self.assertAlmostEqual(raw["probs"]["raise"], 3 / 12)

    def test_sizes_and_all_in(self):
        obs = (_obs("po_lead", {"street": "flop", "role": "aggr", "players": "2", "seat": "last"}, "bet", 3, size=0.5)
               + _obs("po_lead", {"street": "flop", "role": "aggr", "players": "2", "seat": "last"}, "bet", 1,
                      size=0.5, all_in=True)
               + _obs("po_lead", {"street": "flop", "role": "aggr", "players": "2", "seat": "last"}, "check", 4))
        m = pm.fit(obs, k=10.0)
        p = pm.predict(m, "po_lead", {"street": "flop", "role": "aggr", "players": "2", "seat": "last"})
        self.assertAlmostEqual(p["probs"]["bet"], 0.5)
        self.assertEqual(p["size_n"], 4)
        self.assertAlmostEqual(p["sizes"]["0.42-0.62"], 0.75)
        self.assertAlmostEqual(p["sizes"]["allin"], 0.25)

    def test_save_load(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "m.json")
            pm.save(self.m, path)
            back = pm.load(path)
            self.assertEqual(pm.predict(back, "pf_vs_raise", {"seat": "BB", "size": "<=0.42"}),
                             pm.predict(self.m, "pf_vs_raise", {"seat": "BB", "size": "<=0.42"}))
            with open(path) as f:
                bad = json.load(f)
            bad["version"] = 0
            with open(path, "w") as f:
                json.dump(bad, f)
            with self.assertRaises(ValueError):
                pm.load(path)


class Validation(unittest.TestCase):
    def test_splits_do_not_leak(self):
        obs = []
        for i in range(40):
            obs += _obs("pf_unopened", {"pos": "EP"}, "fold", 3, name=f"player{i % 7}", hand_id=f"hand{i}_")
        for i, o in enumerate(obs):
            o.ts = float(i)
        sp = pm.splits(obs)
        for name, (a, b) in sp.items():
            self.assertEqual(len(a) + len(b), len(obs), name)
        a, b = sp["hands"]
        self.assertFalse({o.hand_id for o in a} & {o.hand_id for o in b})
        a, b = sp["players"]
        self.assertFalse({o.name for o in a} & {o.name for o in b})
        a, b = sp["time"]
        self.assertLess(max(o.ts for o in a), min(o.ts for o in b))

    def test_log_loss_of_a_known_model(self):
        fit_obs = _obs("pf_option", {"limpers": "1"}, "check", 3) + _obs("pf_option", {"limpers": "1"}, "raise", 1)
        m = pm.fit(fit_obs, k=10.0)
        test = _obs("pf_option", {"limpers": "1"}, "check", 1) + _obs("pf_option", {"limpers": "1"}, "raise", 1)
        import math

        got = pm.log_loss(m, test)
        self.assertAlmostEqual(got["_all"]["loss"], -(math.log(0.75) + math.log(0.25)) / 2)
        self.assertEqual(got["pf_option"]["n"], 2)

    def test_calibration_of_identical_halves_is_zero(self):
        obs = _obs("pf_option", {"limpers": "1"}, "check", 30) + _obs("pf_option", {"limpers": "1"}, "raise", 10)
        c = pm.calibration(pm.fit(obs, k=10.0), obs)
        self.assertEqual(c["cells"], 1)
        self.assertAlmostEqual(c["mean_abs_gap"], 0.0)
        self.assertAlmostEqual(c["rms_z"], 0.0)

    def test_report_renders(self):
        hands = [hand(LIMP_HAND), hand(FOURBET_HAND)]
        model, val = pm.build(hands)
        self.assertEqual(model["hands"], 2)
        md = pm.render_markdown(model, val)
        self.assertIn("样本外检验", md)
        self.assertIn("翻前：平跟之后被加注", md)
        self.assertIn("翻前各动作亮出来的牌", md)

    def test_command_line_writes_model_and_report(self):
        from opponent_analysis import db
        from opponent_analysis.__main__ import main

        with tempfile.TemporaryDirectory() as d:
            hh = os.path.join(d, "x_hh.jsonl")
            with open(hh, "w") as f:
                for rec in (LIMP_HAND, FOURBET_HAND):
                    f.write(json.dumps(rec) + "\n")
            db.ingest(os.path.join(d, "db"), hh)
            out = os.path.join(d, "out")
            self.assertEqual(main(["pool-model", "--db", os.path.join(d, "db"), "--platform", "poker_manager",
                                   "--out", out]), 0)
            m = pm.load(os.path.join(out, "pool_model.json"))
            self.assertEqual(m["hands"], 2)
            self.assertEqual(m["families"]["pf_vs_raise"]["dims"], ["seat", "size"])
            with open(os.path.join(out, "pool_model.md")) as f:
                self.assertIn("# 牌池模型", f.read())

    def test_combo_groups(self):
        self.assertEqual([pm.combo_group(c) for c in ("AA", "TT", "99", "AKs", "AQo", "AJs", "KTo", "T9s", "72o")],
                         ["strong", "strong", "pair", "strong", "strong", "broadway", "broadway", "other", "other"])


if __name__ == "__main__":
    unittest.main()

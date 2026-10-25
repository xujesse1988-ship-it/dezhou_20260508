import random
import unittest

from opponent_analysis.adapters import poker_manager_raw
from opponent_analysis.replay import replay
from opponent_analysis.report import KEY_RATES, Accumulator, Config, recent_view, render_markdown, trend
from opponent_analysis.tests.test_live import iso, raise_cbet, walk


def hands(raise_probs, seed):
    """One hand per entry: villain "v" on the button opens with that probability, else folds."""
    rng = random.Random(seed)
    out = []
    for i, pr in enumerate(raise_probs):
        names = {0: "v", 1: "w", 2: "hero"}
        rec = (raise_cbet if rng.random() < pr else walk)(f"h{i}", names, iso(i % 3600))
        rec["my_seat"] = 2  # "hero" is ours; "v" and "w" are opponents
        out.append(replay(poker_manager_raw(rec, "t"), heads_up_button_is_sb=True))
    return out


def run(hs, cfg):
    acc = Accumulator("poker_manager")
    for h in hs:
        acc.add(h)
    return acc, acc.finalize([], cfg)


class Recent(unittest.TestCase):
    cfg = Config(min_hands=50, min_opps=10, recent_hands=300)

    def test_window_equals_recomputing_on_last_hands(self):
        hs = hands([0.5] * 700, 1)
        acc, _ = run(hs, self.cfg)
        v, cut = recent_view(acc.players["v"], 300)
        self.assertEqual(cut, 400)
        tail, _ = run(hs[400:], self.cfg)
        for stat, outcome, _ in KEY_RATES:
            self.assertEqual(v.counts(stat), tail.players["v"].counts(stat), stat)
        self.assertEqual(v.hands, 300)
        self.assertAlmostEqual(v.net_bb, tail.players["v"].net_bb)
        whole, c0 = recent_view(acc.players["v"], 5000)
        self.assertIs(whole, acc.players["v"])
        self.assertIsNone(c0)

    def test_style_change_is_flagged(self):
        acc, res = run(hands([0.7] * 900 + [0.15] * 300, 2), self.cfg)
        flagged = {c["label"] for c in res["changes"]["v"] if c["flagged"]}
        self.assertIn("入池率", flagged)
        self.assertIn("首入加注", flagged)
        vpip = next(c for c in res["changes"]["v"] if c["stat"] == "vpip")
        self.assertEqual(vpip["n1"] + vpip["n2"], sum(acc.players["v"].counts("vpip").values()))
        self.assertEqual(vpip["n2"], sum(res["recent"]["v"][0].counts("vpip").values()))
        self.assertLess(vpip["k2"] / vpip["n2"], vpip["k1"] / vpip["n1"])
        self.assertIn("⚠ 入池率", render_markdown(res))

    def test_no_change_no_flag(self):
        _, res = run(hands([0.4] * 1200, 3), self.cfg)
        self.assertGreater(res["n_change_tests"], 0)
        self.assertFalse([c for cs in res["changes"].values() for c in cs if c["flagged"]])

    def test_trend_blocks_add_up(self):
        acc, _ = run(hands([0.5] * 610, 4), self.cfg)
        p = acc.players["v"]
        t = trend(p, bins=4, min_bin=100)
        self.assertEqual(t["bin"], 153)
        self.assertEqual(len(t["ts"]), 4)
        for stat, outcome, _ in KEY_RATES:
            c = p.counts(stat)
            k, n = c.get(outcome, 0), sum(c.values())
            self.assertEqual([sum(x[0] for x in t["series"][f"{stat}:{outcome}"]),
                              sum(x[1] for x in t["series"][f"{stat}:{outcome}"])], [k, n], stat)


if __name__ == "__main__":
    unittest.main()

import math
import random
import unittest

from opponent_analysis import exploit
from opponent_analysis.cards import best_rank, river_percentile
from opponent_analysis.stats import (BetaPrior, K_MAX, benjamini_hochberg, beta_cdf, beta_ppf, fit_prior,
                                     posterior, two_sided_p)


class BetaFunctions(unittest.TestCase):
    def test_closed_forms(self):
        self.assertAlmostEqual(beta_cdf(0.2, 2, 5), 0.34464, places=9)
        self.assertAlmostEqual(beta_ppf(0.3, 1, 1), 0.3, places=9)
        self.assertAlmostEqual(beta_ppf(0.3, 4, 1), 0.3 ** 0.25, places=9)
        self.assertAlmostEqual(beta_ppf(0.5, 7, 7), 0.5, places=9)


class EmpiricalBayes(unittest.TestCase):
    def test_identical_players_get_strong_prior(self):
        rng = random.Random(1)
        samples = [(sum(rng.random() < 0.3 for _ in range(400)), 400) for _ in range(40)]
        pr = fit_prior(samples)
        self.assertAlmostEqual(pr.mean, 0.3, delta=0.02)
        self.assertGreater(pr.strength, 100)

    def test_interval_coverage(self):
        # True rates from Beta(6, 14); 90% posterior intervals should cover ~90% of them.
        rng = random.Random(7)
        truths = [rng.betavariate(6, 14) for _ in range(400)]
        samples = [(sum(rng.random() < p for _ in range(60)), 60) for p in truths]
        pr = fit_prior(samples)
        cover = sum(posterior(k, n, pr).lo <= p <= posterior(k, n, pr).hi for (k, n), p in zip(samples, truths))
        self.assertGreater(cover / len(truths), 0.85)
        self.assertLess(cover / len(truths), 0.95)

    def test_no_false_leaks_when_all_players_equal(self):
        # Null test: 40 players x 20 stats drawn from the same rates -> BH should flag almost nothing.
        rng = random.Random(3)
        pv = []
        for _ in range(40):
            for j in range(20):
                p = 0.1 + 0.04 * j
                n = 200
                k = sum(rng.random() < p for _ in range(n))
                pv.append(two_sided_p(k, n, p))
        self.assertLessEqual(sum(benjamini_hochberg(pv, 0.10)), 2)

    def test_binomial_test_is_exact(self):
        def brute(k, n, p):
            pmf = [math.comb(n, i) * p ** i * (1 - p) ** (n - i) for i in range(n + 1)]
            return min(1.0, 2 * min(sum(pmf[k:]), sum(pmf[:k + 1])))

        for k, n, p in [(3, 10, 0.05), (0, 10, 0.05), (10, 10, 0.9), (5, 10, 0.5), (40, 200, 0.3), (1, 1, 0.5)]:
            self.assertAlmostEqual(two_sided_p(k, n, p), brute(k, n, p), places=12)
        self.assertGreater(two_sided_p(3, 10, 0.05), 0.02)  # the normal approximation said 3e-4

    def test_bh_keeps_real_signal(self):
        self.assertEqual(benjamini_hochberg([1e-6, 0.5, 0.8, 0.9], 0.1), [True, False, False, False])

    def test_prior_shrinks_small_samples(self):
        pr = BetaPrior(0.3, 50)
        self.assertLess(posterior(5, 5, pr).mean, 0.5)
        self.assertAlmostEqual(posterior(0, 0, pr).mean, 0.3)
        self.assertEqual(K_MAX, 200.0)


class Formulas(unittest.TestCase):
    def test_breakevens_are_zero_gain(self):
        pots, bets = [10.0, 6.0], [5.0, 6.0]
        self.assertAlmostEqual(exploit.overfold_gain(exploit.overfold_breakeven(pots, bets), pots, bets), 0.0)
        self.assertAlmostEqual(exploit.overcall_gain(exploit.overcall_breakeven(pots, bets), pots, bets), 0.0)
        self.assertAlmostEqual(exploit.bluffcatch_ev(exploit.bluffcatch_breakeven(pots, bets), pots, bets), 0.0)

    def test_pot_sized_textbook_values(self):
        # Pot-sized bet: bluff needs 50% folds; bluff-catcher needs 1/3 bluffs.
        self.assertAlmostEqual(exploit.overfold_breakeven([1.0], [1.0]), 0.5)
        self.assertAlmostEqual(exploit.bluffcatch_breakeven([1.0], [1.0]), 1 / 3)
        self.assertAlmostEqual(exploit.overfold_gain(0.75, [10.0], [10.0]), 0.75 * 10 - 0.25 * 10)


class Cards(unittest.TestCase):
    def test_category_order(self):
        sf = best_rank(["9h", "8h", "7h", "6h", "5h", "2c", "3d"])
        quads = best_rank(["9h", "9d", "9c", "9s", "5h", "2c", "3d"])
        wheel = best_rank(["Ah", "2d", "3c", "4s", "5h", "Kc", "Qd"])
        six_high = best_rank(["6h", "2d", "3c", "4s", "5h", "Kc", "Qd"])
        self.assertGreater(sf, quads)
        self.assertGreater(six_high, wheel)
        self.assertEqual(wheel[0], 4)

    def test_percentile_extremes(self):
        self.assertEqual(river_percentile(["Ah", "Kh"], ["Qh", "Jh", "Th", "2c", "3d"]), 1.0)
        self.assertLess(river_percentile(["2c", "3d"], ["Ah", "Ks", "9c", "Td", "7h"]), 0.1)


if __name__ == "__main__":
    unittest.main()

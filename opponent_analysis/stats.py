"""Beta posteriors, empirical-Bayes population priors, multiple-testing control."""

from __future__ import annotations

import math
from dataclasses import dataclass


def _betacf(a: float, b: float, x: float) -> float:
    """Continued fraction for the regularized incomplete beta (modified Lentz)."""
    tiny = 1e-300
    qab, qap, qam = a + b, a + 1.0, a - 1.0
    c, d = 1.0, 1.0 - qab * x / qap
    d = 1.0 / (d if abs(d) > tiny else tiny)
    h = d
    for m in range(1, 500):
        m2 = 2 * m
        aa = m * (b - m) * x / ((qam + m2) * (a + m2))
        d = 1.0 + aa * d
        d = 1.0 / (d if abs(d) > tiny else tiny)
        c = 1.0 + aa / c
        c = c if abs(c) > tiny else tiny
        h *= d * c
        aa = -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2))
        d = 1.0 + aa * d
        d = 1.0 / (d if abs(d) > tiny else tiny)
        c = 1.0 + aa / c
        c = c if abs(c) > tiny else tiny
        delta = d * c
        h *= delta
        if abs(delta - 1.0) < 1e-12:
            break
    return h


def beta_cdf(x: float, a: float, b: float) -> float:
    if x <= 0.0:
        return 0.0
    if x >= 1.0:
        return 1.0
    lbeta = math.lgamma(a + b) - math.lgamma(a) - math.lgamma(b)
    front = math.exp(lbeta + a * math.log(x) + b * math.log1p(-x))
    if x < (a + 1.0) / (a + b + 2.0):
        return front * _betacf(a, b, x) / a
    return 1.0 - front * _betacf(b, a, 1.0 - x) / b


def beta_ppf(q: float, a: float, b: float) -> float:
    lo, hi = 0.0, 1.0
    for _ in range(80):
        mid = 0.5 * (lo + hi)
        if beta_cdf(mid, a, b) < q:
            lo = mid
        else:
            hi = mid
    return 0.5 * (lo + hi)


@dataclass(frozen=True)
class BetaPrior:
    mean: float
    strength: float  # pseudo-observations k; prior = Beta(mean*k, (1-mean)*k)

    @property
    def a(self) -> float:
        return self.mean * self.strength

    @property
    def b(self) -> float:
        return (1.0 - self.mean) * self.strength


@dataclass(frozen=True)
class Posterior:
    k: int
    n: int
    mean: float
    lo: float  # 5% quantile
    hi: float  # 95% quantile
    q10: float
    q90: float

    @property
    def raw(self) -> float:
        return self.k / self.n if self.n else float("nan")


def posterior(k: int, n: int, prior: BetaPrior) -> Posterior:
    a, b = prior.a + k, prior.b + n - k
    return Posterior(k, n, a / (a + b), beta_ppf(0.05, a, b), beta_ppf(0.95, a, b),
                     beta_ppf(0.10, a, b), beta_ppf(0.90, a, b))


K_MIN, K_MAX = 2.0, 200.0


def fit_prior(samples: list[tuple[int, int]], min_n: int = 20) -> BetaPrior:
    """Method-of-moments Beta prior across players. samples = [(successes, trials)].

    Pooled mean from all players; the between-player variance uses players with at least
    `min_n` trials, minus the binomial noise they carry. Strength is clamped to
    [K_MIN, K_MAX]: K_MIN when players spread more than any Beta allows, K_MAX when the
    spread is no more than noise. Fewer than three usable players -> weak prior (K_MIN).
    """
    tot_k = sum(k for k, _ in samples)
    tot_n = sum(n for _, n in samples)
    if tot_n == 0:
        return BetaPrior(0.5, K_MIN)
    m = min(max(tot_k / tot_n, 1e-3), 1 - 1e-3)
    big = [(k, n) for k, n in samples if n >= min_n]
    if len(big) < 3:
        return BetaPrior(m, K_MIN)
    rates = [k / n for k, n in big]
    mean_r = sum(rates) / len(rates)
    var_obs = sum((r - mean_r) ** 2 for r in rates) / (len(rates) - 1)
    noise = sum(m * (1 - m) / n for _, n in big) / len(big)
    var_true = var_obs - noise
    if var_true <= 0:
        return BetaPrior(m, K_MAX)
    k = m * (1 - m) / var_true - 1.0
    return BetaPrior(m, min(max(k, K_MIN), K_MAX))


def norm_sf(z: float) -> float:
    return 0.5 * math.erfc(z / math.sqrt(2.0))


def two_sided_p(k: int, n: int, p0: float) -> float:
    if n == 0 or p0 <= 0 or p0 >= 1:
        return 1.0
    z = (k / n - p0) / math.sqrt(p0 * (1 - p0) / n)
    return 2.0 * norm_sf(abs(z))


def benjamini_hochberg(pvals: list[float], q: float) -> list[bool]:
    m = len(pvals)
    order = sorted(range(m), key=lambda i: pvals[i])
    cutoff = -1
    for rank, i in enumerate(order, 1):
        if pvals[i] <= q * rank / m:
            cutoff = rank
    keep = [False] * m
    for rank, i in enumerate(order, 1):
        if rank <= cutoff:
            keep[i] = True
    return keep

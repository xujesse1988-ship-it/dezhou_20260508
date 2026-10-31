"""Minimal hand evaluator for showdown-hand percentiles (no dependency on the solver)."""

from __future__ import annotations

from itertools import combinations

RANKS = "23456789TJQKA"
SUITS = "cdhs"
DECK = [r + s for r in RANKS for s in SUITS]


def _parse(c: str) -> tuple[int, str]:
    return RANKS.index(c[0].upper()), c[1].lower()


def rank5(cards: list[tuple[int, str]]) -> tuple:
    """Comparable strength of exactly five cards (higher tuple = stronger)."""
    rs = sorted((r for r, _ in cards), reverse=True)
    counts: dict[int, int] = {}
    for r in rs:
        counts[r] = counts.get(r, 0) + 1
    groups = sorted(counts.items(), key=lambda kv: (kv[1], kv[0]), reverse=True)
    flush = len({s for _, s in cards}) == 1
    uniq = sorted(counts, reverse=True)
    straight_hi = None
    if len(uniq) == 5:
        if uniq[0] - uniq[4] == 4:
            straight_hi = uniq[0]
        elif uniq == [12, 3, 2, 1, 0]:  # wheel A-5
            straight_hi = 3
    if straight_hi is not None and flush:
        return (8, straight_hi)
    if groups[0][1] == 4:
        return (7, groups[0][0], groups[1][0])
    if groups[0][1] == 3 and groups[1][1] == 2:
        return (6, groups[0][0], groups[1][0])
    if flush:
        return (5, *rs)
    if straight_hi is not None:
        return (4, straight_hi)
    if groups[0][1] == 3:
        return (3, groups[0][0], *[g[0] for g in groups[1:]])
    if groups[0][1] == 2 and groups[1][1] == 2:
        return (2, groups[0][0], groups[1][0], groups[2][0])
    if groups[0][1] == 2:
        return (1, groups[0][0], *[g[0] for g in groups[1:]])
    return (0, *rs)


def hand_class(hole: list[str]) -> str:
    """Two hole cards -> 169-class label, high rank first: "QQ", "AKs", "T9o"."""
    (r1, s1), (r2, s2) = sorted((_parse(c) for c in hole), reverse=True)
    if r1 == r2:
        return RANKS[r1] * 2
    return RANKS[r1] + RANKS[r2] + ("s" if s1 == s2 else "o")


def best_rank(cards: list[str]) -> tuple:
    parsed = [_parse(c) for c in cards]
    return max(rank5(list(c)) for c in combinations(parsed, 5))


def river_percentile(hole: list[str], board: list[str]) -> float:
    """Share of all other two-card holdings (board removed) that `hole` beats; ties count half."""
    if len(board) != 5:
        raise ValueError("river board needs five cards")
    used = {c.lower() for c in board} | {c.lower() for c in hole}
    mine = best_rank(hole + board)
    rest = [c for c in DECK if c.lower() not in used]
    beat = tie = total = 0
    for a, b in combinations(rest, 2):
        r = best_rank([a, b] + board)
        total += 1
        if mine > r:
            beat += 1
        elif mine == r:
            tie += 1
    return (beat + 0.5 * tie) / total

"""Hero 胜率（play log 用）：对手手牌按剩余牌均匀随机，蒙特卡洛发完公共牌。

胜率 = hero 在摊牌时平均分到的底池份额（平分算 1/k）。对手只算还没弃牌的（含已全下的）。
「对随机牌」不看对手的行动，对手下过重注时会高估 hero——只当参考，不进决策。
纯 Python 7 张牌评估（整数打分，越大越强），不依赖 solver。
"""

import random

RANKS = "23456789TJQKA"
SUITS = "cdhs"
DEFAULT_TRIALS = 3000


def card_index(card):
    """'Ah' → 0..51（rank * 4 + suit）。"""
    return RANKS.index(card[0].upper()) * 4 + SUITS.index(card[1].lower())


def _straight_high(mask):
    """13 位点数掩码里最大顺子的顶张（0..12），没有 → -1。A 也当 1 用（A2345 顶张 = 3）。"""
    ext = (mask << 1) | (mask >> 12 & 1)  # bit0 = A as low, bit i+1 = rank i
    for hi in range(13, 3, -1):
        if (ext >> (hi - 4)) & 0x1F == 0x1F:
            return hi - 1
    return -1


_STRAIGHT = [_straight_high(m) for m in range(1 << 13)]


def _top_bits(mask, n):
    out = []
    r = 12
    while r >= 0 and len(out) < n:
        if mask >> r & 1:
            out.append(r)
        r -= 1
    return out


def _pack(cat, ranks):
    v = cat
    for i in range(5):
        v = (v << 4) | (ranks[i] if i < len(ranks) else 0)
    return v


def score7(cards):
    """cards = 牌索引（5..7 张）→ 可比较的整数，越大越强，相等 = 平分。"""
    counts = [0] * 13
    suit_masks = [0, 0, 0, 0]
    for c in cards:
        r = c >> 2
        counts[r] += 1
        suit_masks[c & 3] |= 1 << r
    for sm in suit_masks:
        if bin(sm).count("1") >= 5:
            sf = _STRAIGHT[sm]
            if sf >= 0:
                return _pack(8, [sf])
            return _pack(5, _top_bits(sm, 5))
    mask = 0
    quads, trips, pairs = [], [], []
    for r in range(12, -1, -1):
        n = counts[r]
        if n:
            mask |= 1 << r
            if n == 4:
                quads.append(r)
            elif n == 3:
                trips.append(r)
            elif n == 2:
                pairs.append(r)
    if quads:
        q = quads[0]
        return _pack(7, [q] + _top_bits(mask & ~(1 << q), 1))
    if trips and (len(trips) > 1 or pairs):
        t = trips[0]
        p = max(trips[1] if len(trips) > 1 else -1, pairs[0] if pairs else -1)
        return _pack(6, [t, p])
    st = _STRAIGHT[mask]
    if st >= 0:
        return _pack(4, [st])
    if trips:
        t = trips[0]
        return _pack(3, [t] + _top_bits(mask & ~(1 << t), 2))
    if len(pairs) >= 2:
        a, b = pairs[0], pairs[1]
        return _pack(2, [a, b] + _top_bits(mask & ~(1 << a) & ~(1 << b), 1))
    if pairs:
        p = pairs[0]
        return _pack(1, [p] + _top_bits(mask & ~(1 << p), 3))
    return _pack(0, _top_bits(mask, 5))


def equity(hole, board, n_opps, trials=DEFAULT_TRIALS, seed=None):
    """hero 对 n_opps 家随机手牌的胜率（0..1）。hole / board 为 'Ah' 形式。n_opps < 1 → None。"""
    if n_opps < 1 or len(hole) != 2:
        return None
    hero = [card_index(c) for c in hole]
    bd = [card_index(c) for c in board]
    used = set(hero) | set(bd)
    if len(used) != 2 + len(bd):
        raise ValueError(f"重复的牌: {hole} {board}")
    deck = [c for c in range(52) if c not in used]
    need_board = 5 - len(bd)
    draw = need_board + 2 * n_opps
    if draw > len(deck):
        return None
    rng = random.Random(seed)
    total = 0.0
    for _ in range(trials):
        cards = rng.sample(deck, draw)
        full = bd + cards[:need_board]
        mine = score7(hero + full)
        ties = 0
        lost = False
        for i in range(n_opps):
            j = need_board + 2 * i
            s = score7([cards[j], cards[j + 1]] + full)
            if s > mine:
                lost = True
                break
            if s == mine:
                ties += 1
        if not lost:
            total += 1.0 / (1 + ties)
    return total / trials

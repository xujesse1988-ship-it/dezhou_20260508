#!/usr/bin/env python3
"""range ML M0 公共原语：牌编码 / 169 类索引 / 快速手牌评估 / made+draw 特征 /
翻前强度表（MC，缓存）——全部忠实移植自 src/heuristic/{equity,features}.rs。

设计约束：仅 stdlib（本机无 numpy）；正确性优先，速度靠 rank-count 评估器 + 缓存。
索引约定与 Rust 严格同轴：card_id = rank*4+suit（rank 0..12 = 2..A，suit cdhs=0..3）；
canonical_index：pair 0..12 / suited 13..90 / offsuit 91..168（equity.rs:57）。
"""

import json
import os
import random

RANK_CHARS = "23456789TJQKA"
SUIT_CHARS = "cdhs"

# ---------------------------------------------------------------------------
# 牌编码
# ---------------------------------------------------------------------------


def card_id(s):
    """'Ts' → int id（rank*4+suit）。大小写宽容。"""
    r = RANK_CHARS.index(s[0].upper())
    su = SUIT_CHARS.index(s[1].lower())
    return r * 4 + su


def card_str(c):
    return RANK_CHARS[c // 4] + SUIT_CHARS[c % 4]


def _tri(hi, lo):
    return hi * (hi - 1) // 2 + lo


def canonical_index(c1, c2):
    """两张牌 id → 169 类索引（equity.rs:57 逐行同义）。"""
    r1, r2 = c1 // 4, c2 // 4
    s1, s2 = c1 % 4, c2 % 4
    if r1 == r2:
        return r1
    hi, lo = (r1, r2) if r1 > r2 else (r2, r1)
    return (13 + _tri(hi, lo)) if s1 == s2 else (13 + 78 + _tri(hi, lo))


def combo_weight(idx):
    """类内 combo 总数：pair=6 / suited=4 / offsuit=12（equity.rs:100-106）。"""
    if idx < 13:
        return 6
    return 4 if idx < 91 else 12


def class_name(idx):
    """169 类可读名：'TT' / 'AKs' / 'Q7o'。"""
    if idx < 13:
        return RANK_CHARS[idx] * 2
    t = idx - 13 if idx < 91 else idx - 91
    hi = 1
    while _tri(hi + 1, 0) <= t:
        hi += 1
    lo = t - _tri(hi, 0)
    return RANK_CHARS[hi] + RANK_CHARS[lo] + ("s" if idx < 91 else "o")


def _build_class_combos():
    """CLASS_COMBOS[idx] = 该类全部 concrete combo [(a,b)...]（a<b）。"""
    out = [[] for _ in range(169)]
    for a in range(52):
        for b in range(a + 1, 52):
            out[canonical_index(a, b)].append((a, b))
    return out


CLASS_COMBOS = _build_class_combos()
TOTAL_COMBOS = 1326


def representative_hand(idx):
    """169 类代表 concrete 手（equity.rs:292 同义：pair=c/d 花色；suited 同花 c；offsuit c/d）。"""
    if idx < 13:
        return (idx * 4, idx * 4 + 1)
    suited = idx < 91
    t = idx - 13 if suited else idx - 91
    hi = 1
    while _tri(hi + 1, 0) <= t:
        hi += 1
    lo = t - _tri(hi, 0)
    return (hi * 4, lo * 4) if suited else (hi * 4, lo * 4 + 1)


# ---------------------------------------------------------------------------
# 快速手牌评估（5–7 张，返回可比 tuple；类别序同 Rust HandCategory）
# ---------------------------------------------------------------------------

# 顺子掩码（含 wheel A2345）。顶张 rank：wheel=3（5-high）。
_STRAIGHT_MASKS = []
for _hi in range(12, 3, -1):
    _STRAIGHT_MASKS.append((sum(1 << (_hi - k) for k in range(5)), _hi))
_STRAIGHT_MASKS.append(((1 << 12) | 0b1111, 3))  # A-2-3-4-5

CAT_HIGH, CAT_PAIR, CAT_2PAIR, CAT_TRIPS, CAT_STRAIGHT = 0, 1, 2, 3, 4
CAT_FLUSH, CAT_FH, CAT_QUADS, CAT_SF = 5, 6, 7, 8


def _straight_top(mask):
    for m, top in _STRAIGHT_MASKS:
        if mask & m == m:
            return top
    return None


def eval_cards(cards):
    """5–7 张牌 → (category, tiebreak...) 可比 tuple。类别存在性判定在 5–7 张下与
    「最优 5 张」严格一致（标准结论）；tiebreak 取最优 5 张的 kicker 序。"""
    rc = [0] * 13
    sc = [0] * 4
    sm = [0] * 4
    rm = 0
    for c in cards:
        r, s = c // 4, c % 4
        rc[r] += 1
        sc[s] += 1
        sm[s] |= 1 << r
        rm |= 1 << r
    flush_suit = -1
    for s in range(4):
        if sc[s] >= 5:
            flush_suit = s
            break
    if flush_suit >= 0:
        top = _straight_top(sm[flush_suit])
        if top is not None:
            return (CAT_SF, top)
    quads = [r for r in range(12, -1, -1) if rc[r] == 4]
    if quads:
        q = quads[0]
        kick = max(r for r in range(13) if rc[r] > 0 and r != q)
        return (CAT_QUADS, q, kick)
    trips = [r for r in range(12, -1, -1) if rc[r] == 3]
    pairs = [r for r in range(12, -1, -1) if rc[r] == 2]
    if trips and (len(trips) >= 2 or pairs):
        t = trips[0]
        p = trips[1] if len(trips) >= 2 else pairs[0]
        if pairs and len(trips) >= 2:
            p = max(p, pairs[0])
        return (CAT_FH, t, p)
    if flush_suit >= 0:
        fr = sorted((r for r in range(13) if sm[flush_suit] & (1 << r)), reverse=True)[:5]
        return (CAT_FLUSH,) + tuple(fr)
    top = _straight_top(rm)
    if top is not None:
        return (CAT_STRAIGHT, top)
    singles = [r for r in range(12, -1, -1) if rc[r] == 1]
    if trips:
        return (CAT_TRIPS, trips[0]) + tuple(singles[:2])
    if len(pairs) >= 2:
        hi, lo = pairs[0], pairs[1]
        kick = max(r for r in range(13) if rc[r] > 0 and r != hi and r != lo)
        return (CAT_2PAIR, hi, lo, kick)
    if len(pairs) == 1:
        return (CAT_PAIR, pairs[0]) + tuple(singles[:3])
    return (CAT_HIGH,) + tuple(singles[:5])


# ---------------------------------------------------------------------------
# made_class / draws（features.rs 逐义移植）
# ---------------------------------------------------------------------------

MADE_AIR, MADE_WEAK, MADE_MID, MADE_TOP, MADE_STRONG, MADE_MONSTER = 0, 1, 2, 3, 4, 5


def made_class(hole, board):
    """features.rs:98 made_class。hole=(a,b) ids，board=list ids（len 3..5）。"""
    if len(board) < 3:
        return MADE_AIR
    cat = eval_cards(list(hole) + list(board))[0]
    if cat >= CAT_STRAIGHT:
        return MADE_MONSTER
    if cat in (CAT_TRIPS, CAT_2PAIR):
        return MADE_STRONG
    if cat == CAT_PAIR:
        return _classify_pair(hole, board)
    return MADE_AIR


def _classify_pair(hole, board):
    r0, r1 = hole[0] // 4, hole[1] // 4
    ranks = sorted({c // 4 for c in board}, reverse=True)
    top = ranks[0] if ranks else 0
    second = ranks[1] if len(ranks) > 1 else 0
    if r0 == r1:
        if r0 > top:
            return MADE_TOP
        if r0 > second:
            return MADE_MID
        return MADE_WEAK
    hits_top = r0 == top or r1 == top
    hits_second = r0 == second or r1 == second
    hits_any = any(br in (r0, r1) for br in ranks)
    if hits_top:
        return MADE_TOP
    if hits_second:
        return MADE_MID
    if hits_any:
        return MADE_WEAK
    return MADE_AIR  # board 对、hero 未参与


def _has_straight_mask(mask):
    return _straight_top(mask) is not None


def _straight_outs(mask):
    """features.rs:170：补一张成顺的不同 rank 数；已成顺 → 0。"""
    if _has_straight_mask(mask):
        return 0
    outs = 0
    for cand in range(13):
        if mask & (1 << cand):
            continue
        if _has_straight_mask(mask | (1 << cand)):
            outs += 1
    return outs


def draws(hole, board):
    """features.rs:193 draws → (flush_draw, oesd, gutshot, combo)。river/异常无听牌。"""
    if len(board) >= 5 or len(board) < 3:
        return (False, False, False, False)
    sc = [0] * 4
    hs = [False] * 4
    for c in hole:
        sc[c % 4] += 1
        hs[c % 4] = True
    for c in board:
        sc[c % 4] += 1
    fd = any(sc[s] == 4 and hs[s] for s in range(4))
    mask = 0
    for c in list(board) + list(hole):
        mask |= 1 << (c // 4)
    outs = _straight_outs(mask)
    oesd = outs >= 2
    gut = outs == 1
    combo = fd and (oesd or gut)
    return (fd, oesd, gut, combo)


def draw_score(dr):
    """§2.3 d 轴：0.9·combo + 0.6·flush + 0.5·oesd + 0.25·gutshot，夹 [0,1]。"""
    fd, oesd, gut, combo = dr
    d = 0.9 * combo + 0.6 * fd + 0.5 * oesd + 0.25 * gut
    return min(1.0, d)


# ---------------------------------------------------------------------------
# 翻前强度表（equity.rs:88 EquityEngine::new 的 Python 版，MC + JSON 缓存）
# ---------------------------------------------------------------------------


def build_strength_table(samples=8000, seed=20260702, cache_path=None):
    if cache_path and os.path.exists(cache_path):
        with open(cache_path) as f:
            d = json.load(f)
        if d.get("samples") == samples and d.get("seed") == seed:
            return d["strength"]
    rng = random.Random(seed)
    strength = [0.0] * 169
    for idx in range(169):
        c1, c2 = representative_hand(idx)
        deck = [c for c in range(52) if c not in (c1, c2)]
        win = 0.0
        for _ in range(samples):
            seven = rng.sample(deck, 7)
            board = seven[2:]
            hr = eval_cards([c1, c2] + board)
            orr = eval_cards(seven[:2] + board)
            if hr > orr:
                win += 1.0
            elif hr == orr:
                win += 0.5
        strength[idx] = win / samples
    if cache_path:
        os.makedirs(os.path.dirname(cache_path), exist_ok=True)
        with open(cache_path, "w") as f:
            json.dump({"samples": samples, "seed": seed, "strength": strength}, f)
    return strength


def threshold_for_top(strength, frac):
    """equity.rs:125：top frac（按 combo 权重）的最低强度阈值。"""
    frac = max(0.0, min(1.0, frac))
    if frac >= 1.0:
        return 0.0
    order = sorted(((strength[i], combo_weight(i)) for i in range(169)), reverse=True)
    target = max(1.0, frac * TOTAL_COMBOS)
    acc = 0
    last = 1.0
    for s, w in order:
        acc += w
        last = s
        if acc >= target:
            return s
    return last


# ---------------------------------------------------------------------------
# 对手画像分类（opponent.rs:113 classify + :136 range_threshold_shift 移植）
# ---------------------------------------------------------------------------

CLASSIFY_DEFAULTS = dict(min_hands=18, nit_vpip=0.16, station_vpip=0.45, maniac_af=3.5, adjust_cap=0.12)


def classify(stats, cfg=None):
    """stats: {hands, vpip_opp, vpip_cnt, pfr_opp, pfr_cnt, aggr_actions, call_actions}。"""
    c = dict(CLASSIFY_DEFAULTS)
    if cfg:
        c.update(cfg)
    if not stats or stats.get("hands", 0) < c["min_hands"]:
        return "unknown"
    vpip = stats["vpip_cnt"] / stats["vpip_opp"] if stats.get("vpip_opp") else 0.25
    pfr = stats["pfr_cnt"] / stats["pfr_opp"] if stats.get("pfr_opp") else 0.15
    af = stats["aggr_actions"] / stats["call_actions"] if stats.get("call_actions") else 1.5
    gap = max(0.0, vpip - pfr)
    if vpip < c["nit_vpip"]:
        return "nit"
    if vpip > c["station_vpip"] and af < 1.5:
        return "station"
    if af > c["maniac_af"] or (vpip > 0.40 and pfr > 0.30):
        return "maniac"
    if gap > 0.18 and vpip > 0.30:
        return "lag"
    return "tag"


def range_threshold_shift(stats, cfg=None):
    cap = (cfg or {}).get("adjust_cap", CLASSIFY_DEFAULTS["adjust_cap"])
    t = classify(stats, cfg)
    return {"nit": cap, "station": -cap, "maniac": -cap * 0.8, "lag": -cap * 0.4}.get(t, 0.0)


# ---------------------------------------------------------------------------
# 每 board 的 per-class v/d 表（§2.3 两根轴；逐 alive combo 精确均值）
# ---------------------------------------------------------------------------


def class_axes(board, dead_mask):
    """返回 (v[169], d[169], alive[169])。v = mean made_rank/5，d = mean draw_score，
    对 dead_mask（hero 手牌+board 的 bit 集）之外的存活 combo 求均值。"""
    v = [0.0] * 169
    d = [0.0] * 169
    alive = [0] * 169
    bl = list(board)
    for idx in range(169):
        sv = sd = 0.0
        n = 0
        for a, b in CLASS_COMBOS[idx]:
            if dead_mask & (1 << a) or dead_mask & (1 << b):
                continue
            n += 1
            sv += made_class((a, b), bl) / 5.0
            sd += draw_score(draws((a, b), bl))
        alive[idx] = n
        if n:
            v[idx] = sv / n
            d[idx] = sd / n
    return v, d, alive


def alive_counts(dead_mask):
    """无 board 特征、仅存活 combo 计数（翻前 / B0 用）。"""
    alive = [0] * 169
    for idx in range(169):
        n = 0
        for a, b in CLASS_COMBOS[idx]:
            if dead_mask & (1 << a) or dead_mask & (1 << b):
                continue
            n += 1
        alive[idx] = n
    return alive


def dead_mask_of(cards):
    m = 0
    for c in cards:
        m |= 1 << c
    return m


# ---------------------------------------------------------------------------
# 自检
# ---------------------------------------------------------------------------


def _selftest():
    def cid(*ss):
        return [card_id(s) for s in ss]

    # canonical_index 轴向
    assert canonical_index(card_id("As"), card_id("Ah")) == 12
    assert canonical_index(card_id("2c"), card_id("2d")) == 0
    assert class_name(canonical_index(card_id("As"), card_id("Ks"))) == "AKs"
    assert class_name(canonical_index(card_id("Ks"), card_id("Ah"))) == "AKo"
    assert sum(len(c) for c in CLASS_COMBOS) == 1326
    for idx in (0, 12, 13, 90, 91, 168):
        a, b = representative_hand(idx)
        assert canonical_index(a, b) == idx
    # 评估器
    assert eval_cards(cid("As", "Ks", "Qs", "Js", "Ts"))[0] == CAT_SF
    assert eval_cards(cid("Ah", "2h", "3h", "4h", "5h"))[0] == CAT_SF
    assert eval_cards(cid("As", "Ad", "Ac", "Ah", "Ks"))[0] == CAT_QUADS
    assert eval_cards(cid("As", "Ad", "Ac", "Kh", "Ks"))[0] == CAT_FH
    assert eval_cards(cid("As", "Ad", "Ac", "Kh", "Kd", "Kc", "2s"))[0] == CAT_FH
    assert eval_cards(cid("As", "Ks", "2s", "7s", "9s", "3d"))[0] == CAT_FLUSH
    assert eval_cards(cid("Ah", "2s", "3d", "4c", "5h"))[0] == CAT_STRAIGHT
    assert eval_cards(cid("9h", "8s", "7d", "6c", "5h", "2s", "2d"))[0] == CAT_STRAIGHT
    assert eval_cards(cid("As", "Ad", "Kc", "Kh", "2s"))[0] == CAT_2PAIR
    assert eval_cards(cid("As", "Ad", "Kc", "Qh", "2s"))[0] == CAT_PAIR
    assert eval_cards(cid("As", "Jd", "9c", "7h", "2s"))[0] == CAT_HIGH
    # 大小比较（同类别 kicker）
    assert eval_cards(cid("As", "Ad", "Kc", "Qh", "2s")) > eval_cards(cid("As", "Ad", "Kc", "Jh", "3s"))
    # made_class
    b = cid("Tc", "4c", "6h")
    assert made_class(tuple(cid("Td", "As")), b) == MADE_TOP
    assert made_class(tuple(cid("7c", "7s")), b) == MADE_MID  # 口袋 77 > 6,4 但 < T
    assert made_class(tuple(cid("Ad", "Ks")), b) == MADE_AIR
    assert made_class(tuple(cid("4d", "4s")), b) == MADE_STRONG  # set
    b2 = cid("9s", "8d", "2c")
    assert made_class(tuple(cid("7s", "6s")), b2) == MADE_AIR
    fd, oesd, gut, combo = draws(tuple(cid("7s", "6s")), b2)
    assert oesd and not fd  # 76 on 982 两头顺（§2.4 走通例）
    fd2 = draws(tuple(cid("As", "5s")), cid("Ks", "2s", "9d"))
    assert fd2[0]
    # threshold_for_top 单调
    st = [i / 168.0 for i in range(169)]
    assert threshold_for_top(st, 0.08) > threshold_for_top(st, 0.34) > threshold_for_top(st, 0.8)
    # classify
    assert classify({"hands": 30, "vpip_opp": 20, "vpip_cnt": 2, "pfr_opp": 20, "pfr_cnt": 1,
                     "aggr_actions": 2, "call_actions": 2}) == "nit"
    assert classify({"hands": 30, "vpip_opp": 20, "vpip_cnt": 12, "pfr_opp": 20, "pfr_cnt": 1,
                     "aggr_actions": 2, "call_actions": 9}) == "station"
    assert classify({"hands": 5, "vpip_opp": 4, "vpip_cnt": 2, "pfr_opp": 4, "pfr_cnt": 1,
                     "aggr_actions": 1, "call_actions": 1}) == "unknown"
    print("range_ml_common selftest OK")


if __name__ == "__main__":
    import sys

    if "--selftest" in sys.argv:
        _selftest()
    elif "--strength" in sys.argv:
        import time

        t0 = time.time()
        st = build_strength_table(
            samples=int(sys.argv[sys.argv.index("--strength") + 1]) if len(sys.argv) > 2 else 8000,
            cache_path="artifacts/range_ml/strength169.json",
        )
        top = sorted(range(169), key=lambda i: -st[i])[:8]
        print(f"built in {time.time()-t0:.1f}s; top: {[(class_name(i), round(st[i],3)) for i in top]}")

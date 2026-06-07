#!/usr/bin/env python3
"""6-max preopen 10B blueprint 收敛诊断。

1) 各位置 preflop 支配翻转率（域内 kicker/rank 单调性，>0.05 阈）。
2) SB 2B->10B 每手 L1 漂移（回答文档悬而未决：SB 欠训练 vs 抽象天花板）。
3) 用文档 SB-2B 数据校准翻转率定义（目标 ~17.1%）后套到 10B。

只读 markdown，纯分析，不碰 solver。
"""
import re
import sys

RANKS = "AKQJT98765432"
RIDX = {r: i for i, r in enumerate(RANKS)}  # A=0 (strongest) .. 2=12


def hand_rank_key(h):
    """canonical hand -> sort key within family (strongest first)."""
    if len(h) == 2:  # pair
        return RIDX[h[0]]
    return (RIDX[h[0]], RIDX[h[1]])


def families():
    """返回支配族列表：每族是按强度（kicker/rank desc）排好的 canonical hand 列表。
    - pairs：AA..22（rank 支配）
    - 每个高牌 X 的 suited 族 Xks（kicker desc）
    - 每个高牌 X 的 offsuit 族 Xko（kicker desc）
    族内严格支配 = 排在前的 dominate 排在后的（equity 单调）。"""
    fams = []
    # pairs
    fams.append([r + r for r in RANKS])
    # suited / offsuit by high card
    for hi_i, hi in enumerate(RANKS):
        suited = [hi + lo + "s" for lo in RANKS[hi_i + 1:]]
        offs = [hi + lo + "o" for lo in RANKS[hi_i + 1:]]
        if len(suited) >= 2:
            fams.append(suited)
        if len(offs) >= 2:
            fams.append(offs)
    return fams


FAMS = families()


def raise_freq(dist):
    """开池（aggression）频率 = 所有 Raise 列之和。"""
    return sum(v for k, v in dist.items() if "Raise" in k)


def flip_rate(strat, thresh=0.05, mode="all"):
    """strat: {hand: {col: prob}}。返回 (flip_rate, flips, pairs)。
    mode='all' = 族内所有有序支配对；mode='adj' = 仅相邻对。"""
    flips = 0
    pairs = 0
    for fam in FAMS:
        present = [h for h in fam if h in strat]
        n = len(present)
        if mode == "adj":
            idx_pairs = [(i, i + 1) for i in range(n - 1)]
        else:
            idx_pairs = [(i, j) for i in range(n) for j in range(i + 1, n)]
        for i, j in idx_pairs:
            dom = raise_freq(strat[present[i]])   # 强
            sub = raise_freq(strat[present[j]])   # 弱
            pairs += 1
            if sub - dom > thresh:
                flips += 1
    return (flips / pairs if pairs else 0.0, flips, pairs)


# ---------- parse 10B dump ----------
def parse_dump(path):
    positions = {}
    cur = None
    cols = None
    for line in open(path):
        line = line.rstrip("\n")
        m = re.match(r"## (\w+) RFI", line)
        if m:
            cur = m.group(1)
            positions[cur] = {}
            cols = None
            continue
        if cur and line.startswith("- legal_actions:"):
            raw = line.split(":", 1)[1].strip()
            cols = [c.strip() for c in raw.split("|")]
            continue
        if cur and cols and line.startswith("|"):
            cells = [c.strip() for c in line.strip().strip("|").split("|")]
            if len(cells) < 2 + len(cols):
                continue
            hand = cells[0]
            if hand in ("hand",) or hand.startswith("-"):
                continue
            try:
                vals = [float(x) for x in cells[2:2 + len(cols)]]
            except ValueError:
                continue
            positions[cur][hand] = dict(zip(cols, vals))
    return positions


# ---------- parse doc SB 1B/2B ----------
def parse_sb_doc(path):
    """returns sb_1b, sb_2b : {hand: {'F':..,'L':..,'2.25':..,'3.5':..}}
    doc 列顺序: 手 | 2B L | 2B 2.25 | 2B 3.5 | 2B F | 1B L | 1B 2.25 | 1B 3.5 | 1B F"""
    sb_1b, sb_2b = {}, {}
    hand_re = re.compile(r"^[AKQJT2-9]{2}[so]?$")
    for line in open(path):
        line = line.strip()
        if not line.startswith("|"):
            continue
        cells = [c.strip() for c in line.strip("|").split("|")]
        if len(cells) != 9:
            continue
        hand = cells[0]
        if not hand_re.match(hand):
            continue
        try:
            v = [float(x) for x in cells[1:9]]
        except ValueError:
            continue
        sb_2b[hand] = {"L": v[0], "2.25": v[1], "3.5": v[2], "F": v[3]}
        sb_1b[hand] = {"L": v[4], "2.25": v[5], "3.5": v[6], "F": v[7]}
    return sb_1b, sb_2b


def sb_doc_to_raisefmt(sb):
    """doc 四列 -> 与 dump 同构的 {col: prob}，列名带 Raise 以复用 raise_freq。"""
    out = {}
    for h, d in sb.items():
        out[h] = {"Fold": d["F"], "Call/Limp": d["L"],
                  "Raise(0.5x)": d["2.25"], "Raise(1x)": d["3.5"]}
    return out


def sb_4col(dist, is_dump):
    """规约成 (F,L,2.25,3.5) 4-tuple 便于算漂移。"""
    if is_dump:
        return (dist.get("Fold", 0.0), dist.get("Call/Limp", 0.0),
                dist.get("Raise(0.5x)", 0.0), dist.get("Raise(1x)", 0.0))
    return (dist["F"], dist["L"], dist["2.25"], dist["3.5"])


def main():
    dump_path = sys.argv[1]
    sb_doc_path = sys.argv[2]
    pos = parse_dump(dump_path)
    sb_1b, sb_2b = parse_sb_doc(sb_doc_path)

    print("=" * 70)
    print("CALIBRATION: doc SB-2B flip rate (target doc=17.1%)")
    sb2b_rf = sb_doc_to_raisefmt(sb_2b)
    sb1b_rf = sb_doc_to_raisefmt(sb_1b)
    for mode in ("all", "adj"):
        r2, f2, p2 = flip_rate(sb2b_rf, mode=mode)
        r1, f1, p1 = flip_rate(sb1b_rf, mode=mode)
        print(f"  mode={mode:3s}  SB-1B={r1*100:5.2f}% ({f1}/{p1})   "
              f"SB-2B={r2*100:5.2f}% ({f2}/{p2})   [doc 1B=17.7 2B=17.1]")

    print("=" * 70)
    print("10B preopen — per-position flip rate (mode=all, thresh=0.05)")
    print(f"  {'pos':4s} {'flip%':>7s} {'flips/pairs':>12s}   "
          f"{'doc preopen-2B':>16s}")
    doc_ref = {"UTG": "0.2", "HJ": "0.2", "CO": "0.2", "BTN": "0.5", "SB": "17.1"}
    order = ["UTG", "HJ", "CO", "BTN", "SB"]
    for p in order:
        if p not in pos:
            continue
        r, f, pr = flip_rate(pos[p], mode="all")
        print(f"  {p:4s} {r*100:6.2f}% {f:5d}/{pr:<6d}   {doc_ref.get(p,''):>16s}")
    # non-blind aggregate
    nb = {}
    for p in ["UTG", "HJ", "CO", "BTN"]:
        nb.update({f"{p}:{h}": d for h, d in pos.get(p, {}).items()})

    print("=" * 70)
    print("SB convergence: per-hand L1 drift")

    def avg_drift(a_map, b_map, a_dump, b_dump):
        ds = []
        hands = set(a_map) & set(b_map)
        for h in hands:
            a = sb_4col(a_map[h], a_dump)
            b = sb_4col(b_map[h], b_dump)
            ds.append(sum(abs(x - y) for x, y in zip(a, b)))
        ds.sort()
        n = len(ds)
        return (sum(ds) / n, ds[n // 2], max(ds), n,
                sum(1 for d in ds if d > 0.3))

    sb10 = pos.get("SB", {})
    # 1B->2B (doc, both 4col)
    m, med, mx, n, big = avg_drift(sb_1b, sb_2b, False, False)
    print(f"  SB 1B->2B : mean={m:.3f} median={med:.3f} max={mx:.3f} "
          f"n={n} (>0.3: {big})   [doc mean=0.284]")
    # 2B->10B
    m, med, mx, n, big = avg_drift(sb_2b, sb10, False, True)
    print(f"  SB 2B->10B: mean={m:.3f} median={med:.3f} max={mx:.3f} "
          f"n={n} (>0.3: {big})")

    def combos(h):
        if len(h) == 2:
            return 6      # pair
        return 4 if h.endswith("s") else 12

    print("=" * 70)
    print("10B per-position summary (combo-WEIGHTED, like doc)")
    print(f"  {'pos':4s} {'limp%':>6s} {'raise%':>7s} {'vpip%':>6s} "
          f"{'fold100':>8s}")
    for p in order:
        if p not in pos:
            continue
        hs = pos[p]
        w = {h: combos(h) for h in hs}
        wsum = sum(w.values())
        limp = sum(w[h] * d.get("Call/Limp", 0.0) for h, d in hs.items()) / wsum
        rai = sum(w[h] * raise_freq(d) for h, d in hs.items()) / wsum
        vpip = limp + rai
        fold100 = sum(1 for d in hs.values() if d.get("Fold", 0.0) >= 0.999)
        print(f"  {p:4s} {limp*100:5.1f}% {rai*100:6.1f}% {vpip*100:5.1f}% "
              f"{fold100:5d}/{len(hs)}")

    print("=" * 70)
    print("Premium-hand monotonicity spot-check (10B, raise=open freq)")
    for p in order:
        if p not in pos:
            continue
        hs = pos[p]
        prem = ["AA", "KK", "QQ", "JJ", "AKs", "AKo"]
        vals = []
        for h in prem:
            if h in hs:
                vals.append(f"{h}={raise_freq(hs[h]):.2f}")
        print(f"  {p:4s} " + "  ".join(vals))


if __name__ == "__main__":
    main()

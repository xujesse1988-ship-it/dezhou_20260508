"""Pool model: how the opponents of one platform play on average, as frequencies with counts.

Every non-hero decision of every stored hand is put in exactly one situation ("family") and one
cell of that family; a cell counts what was done there (fold / call / raise, check / bet) and, for
bets and raises, how big. Nothing here looks at hole cards except `shown`, which only tallies the
hands players showed after each kind of decision (a biased sample, kept as calibration data).

The model is the average over players, weighted by how often each one acts. A mixture of players
is a single strategy only when a cell fixes the acting player's own earlier actions in the hand,
so the families split on them (limped before, raised before, was the aggressor of the last street).

Estimates back off from fine cells to coarse ones: a cell with the first j dimensions fixed is
smoothed toward the cell with the first j-1 fixed,

    p_j(a) = (count_j(a) + k * p_{j-1}(a)) / (n_j + k),   p_0 = family total,

so a cell nobody has been seen in answers with the next coarser one. `n` of the finest cell is
returned with every answer: the consumer decides how far to trust it.

Sizes are pot fractions, the unit of the solver's betting tree:
    (amount raised above the call) / (pot before the action + the call).
With 8 seats and a 1 BB ante an open to 4.5 BB is 1/3 pot and an open to 11.5 BB is 1 pot.
"""

from __future__ import annotations

import json
import math
from collections import Counter, defaultdict
from dataclasses import dataclass
from typing import Iterable, Optional

from .cards import hand_class
from .model import Hand
from .replay import ring_from

VERSION = 1

# Upper edges of the size buckets (pot fractions). The tree's usual sizes fall in different
# buckets: 1/3, 1/2, 2/3-3/4, 1 pot, overbet, far overbet.
SIZE_EDGES = (0.42, 0.62, 0.87, 1.2, 2.0)
SIZE_LABELS = ("<=0.42", "0.42-0.62", "0.62-0.87", "0.87-1.2", "1.2-2.0", ">2.0")

# Family -> the dimensions its cells are split on, most important first (back-off drops them from
# the right). `hand_obs` records more features than are used here; which ones earn a place was
# decided on held-out data (`validate`): a dimension stays only if it lowers the held-out log loss
# on all three splits. Dropped that way: callers behind a raise, open vs isolation raise, number of
# limpers, street when facing a bet.
FAMILIES: dict[str, tuple[str, ...]] = {
    # Preflop, no raise yet.
    "pf_unopened": ("pos",),  # nobody has limped; the acting seat owes chips
    "pf_vs_limp": ("pos",),  # one or more limpers in front
    "pf_option": ("limpers",),  # nothing to call (big blind, live-blind poster)
    # Preflop, one raise.
    "pf_vs_raise": ("seat", "size"),  # has not put chips in voluntarily
    "pf_limper_vs_raise": ("size",),  # limped (or checked the option) before the raise
    # Preflop, two raises.
    "pf_raiser_vs_3bet": ("size",),  # made the first raise
    "pf_other_vs_3bet": ("entered",),  # did not raise: cold, or called / limped before
    # Preflop, three or more raises.
    "pf_vs_4bet": ("raised",),
    # Postflop.
    "po_lead": ("role", "seat", "players", "street"),  # no bet yet on this street
    "po_vs_bet": ("players", "size"),  # facing the first bet of the street
    "po_vs_raise": ("was_bettor",),  # facing a raise
}

# Dimensions the size of a bet / raise is split on (looked up with the same back-off).
SIZE_DIMS: dict[str, tuple[str, ...]] = {
    "pf_unopened": ("pos",),
    "pf_vs_limp": ("limpers",),
    "pf_option": (),
    "pf_vs_raise": ("seat",),
    "pf_limper_vs_raise": (),
    "pf_raiser_vs_3bet": (),
    "pf_other_vs_3bet": (),
    "pf_vs_4bet": (),
    "po_lead": ("street", "role"),
    "po_vs_bet": ("street",),
    "po_vs_raise": (),
}

# Pseudo-observations of the coarser cell mixed into a finer one. 10 is near the best held-out
# log loss on every split; the loss is flat between 5 and 50.
DEFAULT_K = 10.0


def size_bucket(frac: float) -> str:
    for edge, label in zip(SIZE_EDGES, SIZE_LABELS):
        if frac <= edge:
            return label
    return SIZE_LABELS[-1]


def raise_fraction(to_total: int, current_bet: int, pot_before: int, to_call: int) -> float:
    """Pot fraction of a bet / raise: chips above the call over the pot after calling."""
    base = pot_before + to_call
    return (to_total - current_bet) / base if base > 0 else 0.0


@dataclass
class PoolObs:
    """One opponent decision: where it was made, what was done."""

    hand_id: str
    ts: Optional[float]
    name: str
    family: str
    dims: dict
    outcome: str  # fold / call / raise (preflop, facing a bet), check / bet (po_lead, pf_option: check / raise)
    size: Optional[float] = None  # pot fraction of the bet / raise
    all_in: bool = False
    combo: Optional[str] = None  # hand class when the player showed his cards


def _pos_group(label: str) -> str:
    if label in ("BTN", "BTN_SB"):
        return "BTN"
    if label in ("SB", "BB", "CO"):
        return label
    if label == "BTN_BB":
        return "BB"
    if label in ("HJ", "LJ"):
        return "MP"
    return "EP"


def _seat3(label: str) -> str:
    """Seat of a player facing a raise: the two blinds have chips in and act last; the rest are 'field'."""
    if label in ("SB", "BTN_SB"):
        return "SB"
    if label in ("BB", "BTN_BB"):
        return "BB"
    return "field"


def hand_obs(h: Hand, hero_names: Iterable[str] = ()) -> list[PoolObs]:
    """All pool observations of one hand (hero seats and server-forced actions left out)."""
    heroes = set(hero_names)
    hero_nm = h.names.get(h.hero_seat)
    if hero_nm:
        heroes.add(hero_nm)
    out: list[PoolObs] = []

    street = None
    own: dict[int, list[str]] = {}  # seat -> kinds on the current street
    last_frac = 0.0  # pot fraction of the last bet / raise on the street
    limped_pot = False  # preflop: someone called before the first raise
    raisers: list[int] = []  # preflop raisers in order
    prev_aggr: Optional[int] = None  # last bettor / raiser of the previous street
    cur_aggr: Optional[int] = None
    folded: set[int] = set()
    all_in: set[int] = set()
    live_order: list[int] = []  # seats that can still act on this street, in acting order

    for d in h.decisions:
        if d.street != street:
            street = d.street
            own = {}
            last_frac = 0.0
            prev_aggr, cur_aggr = cur_aggr, None
            # Postflop order: first seat after the button acts first.
            live_order = [s for s in ring_from(h.dealt_seats, h.button_seat)[1:] + [h.button_seat]
                          if s not in folded and s not in all_in]
        mine = own.setdefault(d.seat, [])
        nm = h.names.get(d.seat)
        skip = (not nm) or nm in heroes or d.seat == h.hero_seat or bool(d.forced)
        pos = h.positions.get(d.seat, "?")
        fam: Optional[str] = None
        dims: dict = {}
        if d.street == "preflop":
            rb = d.raises_before
            if rb == 0:
                if d.to_call == 0:
                    fam, dims = "pf_option", {"limpers": "0-1" if d.prior_callers <= 1 else "2+"}
                elif d.prior_callers == 0:
                    fam, dims = "pf_unopened", {"pos": _pos_group(pos)}
                else:
                    fam = "pf_vs_limp"
                    dims = {"pos": _pos_group(pos), "limpers": "1" if d.prior_callers == 1 else "2+"}
            elif rb == 1:
                size = size_bucket(last_frac)
                callers = "0" if d.prior_callers == 0 else "1+"
                if mine:  # limped, or checked the option, before the raise
                    fam, dims = "pf_limper_vs_raise", {"size": size, "callers": callers}
                else:
                    fam = "pf_vs_raise"
                    dims = {"size": size, "seat": _seat3(pos), "callers": callers,
                            "raise_kind": "iso" if limped_pot else "open"}
            elif rb == 2:
                if raisers and raisers[0] == d.seat:
                    fam = "pf_raiser_vs_3bet"
                    dims = {"size": size_bucket(last_frac), "callers": "0" if d.prior_callers == 0 else "1+"}
                else:
                    fam, dims = "pf_other_vs_3bet", {"entered": "yes" if mine else "no"}
            else:
                fam, dims = "pf_vs_4bet", {"raised": "yes" if d.seat in raisers else "no"}
        else:
            players = "2" if d.n_live == 2 else "3+"
            if d.raises_before == 0:
                role = "aggr" if prev_aggr == d.seat else ("none" if prev_aggr is None else "other")
                last = live_order[-1] if live_order else None
                fam = "po_lead"
                dims = {"street": d.street, "role": role, "players": players, "seat": "last" if last == d.seat else "early"}
            elif d.raises_before == 1:
                fam, dims = "po_vs_bet", {"size": size_bucket(last_frac), "street": d.street, "players": players}
            else:
                # The bettor facing a raise, or anyone else facing bet + raise.
                fam = "po_vs_raise"
                dims = {"street": d.street, "was_bettor": "yes" if any(k in ("bet", "raise") for k in mine) else "no"}

        frac = None
        if d.kind in ("bet", "raise"):
            frac = raise_fraction(d.to_total, d.current_bet, d.pot_before, d.to_call)
        if not skip and fam is not None:
            if fam in ("po_lead",):
                outcome = "bet" if d.kind in ("bet", "raise") else "check"
            elif fam == "pf_option":
                outcome = "raise" if d.kind == "raise" else "check"
            else:
                outcome = {"fold": "fold", "call": "call", "check": "call"}.get(d.kind, "raise")
            cards = h.shown.get(d.seat)
            out.append(PoolObs(h.hand_id, h.ts, nm, fam, dims, outcome, frac, d.all_in,
                               hand_class(cards) if cards else None))

        # State after the action.
        mine.append(d.kind)
        if d.kind == "fold":
            folded.add(d.seat)
        if d.all_in:
            all_in.add(d.seat)
        if d.kind in ("bet", "raise"):
            last_frac = frac or 0.0
            cur_aggr = d.seat
            if d.street == "preflop":
                raisers.append(d.seat)
        elif d.street == "preflop" and d.kind == "call" and d.raises_before == 0:
            limped_pot = True
    return out


def collect(hands: Iterable[Hand], hero_names: Iterable[str] = ()) -> list[PoolObs]:
    out: list[PoolObs] = []
    for h in hands:
        out.extend(hand_obs(h, hero_names))
    return out


# ---------------------------------------------------------------- fit / predict


def _key(dims: dict, names: tuple[str, ...], depth: int) -> str:
    return "|".join(str(dims[n]) for n in names[:depth])


def fit(obs: Iterable[PoolObs], k: float = DEFAULT_K) -> dict:
    """Count every observation into all back-off levels of its family. Pure counts plus `k`."""
    fams: dict = {f: {"dims": list(ds), "cells": defaultdict(Counter), "size_dims": list(SIZE_DIMS[f]),
                      "size_cells": defaultdict(Counter)} for f, ds in FAMILIES.items()}
    shown: dict = defaultdict(Counter)
    n_obs = 0
    hands = set()
    players = set()
    ts = []
    for o in obs:
        n_obs += 1
        hands.add(o.hand_id)
        players.add(o.name)
        if o.ts is not None:
            ts.append(o.ts)
        f = fams[o.family]
        names = FAMILIES[o.family]
        for depth in range(len(names) + 1):
            f["cells"][_key(o.dims, names, depth)][o.outcome] += 1
        if o.size is not None and o.outcome in ("bet", "raise"):
            snames = SIZE_DIMS[o.family]
            label = "allin" if o.all_in else size_bucket(o.size)
            for depth in range(len(snames) + 1):
                f["size_cells"][_key(o.dims, snames, depth)][label] += 1
        if o.combo and o.family.startswith("pf_"):  # a 169-class says nothing about postflop strength
            shown[f"{o.family}|{o.outcome}"][o.combo] += 1
    return {
        "version": VERSION, "k": k, "size_edges": list(SIZE_EDGES), "size_labels": list(SIZE_LABELS) + ["allin"],
        "observations": n_obs, "hands": len(hands), "players": len(players),
        "ts_range": [min(ts), max(ts)] if ts else None,
        "families": {name: {"dims": f["dims"], "cells": {c: dict(v) for c, v in sorted(f["cells"].items())},
                            "size_dims": f["size_dims"],
                            "size_cells": {c: dict(v) for c, v in sorted(f["size_cells"].items())}}
                     for name, f in fams.items()},
        "shown": {key: dict(v) for key, v in sorted(shown.items())},
    }


def _smooth(cells: dict, names: list[str], dims: dict, k: float, depth: Optional[int] = None) -> tuple[dict, int]:
    """Backed-off probabilities at `depth` (default: all dimensions) and the count of that cell."""
    depth = len(names) if depth is None else depth
    root = cells.get("", {})
    total = sum(root.values())
    if total == 0:
        return {}, 0
    probs = {a: c / total for a, c in root.items()}
    n = total
    for j in range(1, depth + 1):
        cell = cells.get(_key(dims, tuple(names), j), {})
        n = sum(cell.values())
        probs = {a: (cell.get(a, 0) + k * p) / (n + k) for a, p in probs.items()}
    return probs, n


def predict(model: dict, family: str, dims: dict, k: Optional[float] = None, depth: Optional[int] = None) -> dict:
    """Action probabilities in one cell.

    Returns {"probs": {outcome: p}, "n": observations in the finest cell asked for,
             "sizes": {size label: p given a bet / raise}, "size_n": ...}.
    """
    k = model["k"] if k is None else k
    f = model["families"][family]
    probs, n = _smooth(f["cells"], f["dims"], dims, k, depth)
    sizes, size_n = _smooth(f["size_cells"], f["size_dims"], dims, k)
    return {"probs": probs, "n": n, "sizes": sizes, "size_n": size_n}


# ---------------------------------------------------------------- validation


def log_loss(model: dict, obs: Iterable[PoolObs], k: Optional[float] = None, depth: Optional[int] = None,
             eps: float = 1e-3) -> dict:
    """Mean negative log likelihood (nats) of held-out decisions, per family and overall.

    An outcome the model never saw in the family gets probability `eps` (the rest rescaled).
    """
    tot: dict = defaultdict(lambda: [0.0, 0])
    for o in obs:
        p = predict(model, o.family, o.dims, k, depth)["probs"]
        q = p.get(o.outcome, 0.0)
        q = eps + (1 - eps * 3) * q if q < eps else q
        tot[o.family][0] += -math.log(q)
        tot[o.family][1] += 1
    res = {f: {"loss": s / n, "n": n} for f, (s, n) in tot.items() if n}
    s = sum(v[0] for v in tot.values())
    n = sum(v[1] for v in tot.values())
    res["_all"] = {"loss": s / n if n else float("nan"), "n": n}
    return res


def save(model: dict, path: str) -> None:
    with open(path, "w") as f:
        json.dump(model, f, ensure_ascii=False, indent=1, sort_keys=True)
        f.write("\n")


def load(path: str) -> dict:
    with open(path) as f:
        m = json.load(f)
    if m.get("version") != VERSION:
        raise ValueError(f"pool model version {m.get('version')} != {VERSION}")
    return m


# ---------------------------------------------------------------- hold-out checks


def _half(text: str) -> int:
    """Stable 0 / 1 split of a string (not Python's salted hash)."""
    import hashlib

    return hashlib.sha256(text.encode()).digest()[0] & 1


def splits(obs: list[PoolObs]) -> dict[str, tuple[list[PoolObs], list[PoolObs]]]:
    """Named (fit, test) splits.

    - hands: a random half of the hands predicts the other half (same players on both sides).
    - time:  the earlier 60% of the hands predicts the later 40%.
    - players: a random half of the players predicts the other half (people never seen in the fit).
    """
    ids = sorted({(o.ts or 0.0, o.hand_id) for o in obs})
    cut = ids[int(len(ids) * 0.6)][0] if ids else 0.0
    return {
        "hands": ([o for o in obs if _half(o.hand_id) == 0], [o for o in obs if _half(o.hand_id) == 1]),
        "time": ([o for o in obs if (o.ts or 0.0) < cut], [o for o in obs if (o.ts or 0.0) >= cut]),
        "players": ([o for o in obs if _half(o.name) == 0], [o for o in obs if _half(o.name) == 1]),
    }


def calibration(model: dict, test: list[PoolObs], min_n: int = 30) -> dict:
    """How far the finest cells' predicted frequencies are from what the held-out half did.

    For every (family, finest cell, outcome) with at least `min_n` held-out decisions in the cell:
    predicted p, held-out rate r, and z = (r - p) / sqrt(p (1 - p) (1 / n_test + 1 / (n_fit + k))),
    the gap in units of the sampling noise of both halves. Returns the mean absolute gap weighted
    by held-out decisions, the root mean square z (1.0 = gaps are pure sampling noise) and the rows.
    """
    by_cell: dict = defaultdict(Counter)
    dims_of: dict = {}
    for o in test:
        key = (o.family, _key(o.dims, FAMILIES[o.family], len(FAMILIES[o.family])))
        by_cell[key][o.outcome] += 1
        dims_of[key] = o.dims
    rows = []
    for (fam, cell), cnt in by_cell.items():
        n_test = sum(cnt.values())
        if n_test < min_n:
            continue
        pr = predict(model, fam, dims_of[(fam, cell)])
        for a, p in pr["probs"].items():
            r = cnt.get(a, 0) / n_test
            var = p * (1 - p) * (1 / n_test + 1 / (pr["n"] + model["k"]))
            z = (r - p) / math.sqrt(var) if var > 0 else 0.0
            rows.append({"family": fam, "cell": cell, "outcome": a, "pred": p, "actual": r,
                         "n_fit": pr["n"], "n_test": n_test, "z": z})
    w = sum(r["n_test"] for r in rows)
    mae = sum(abs(r["actual"] - r["pred"]) * r["n_test"] for r in rows) / w if w else float("nan")
    rms = math.sqrt(sum(r["z"] ** 2 for r in rows) / len(rows)) if rows else float("nan")
    return {"mean_abs_gap": mae, "rms_z": rms, "cells": len({(r["family"], r["cell"]) for r in rows}), "rows": rows}


def validate(obs: list[PoolObs], ks=(1.0, 2.0, 5.0, 10.0, 20.0, 50.0)) -> dict:
    """Hold-out results for every split: log loss of the back-off model at each k, of the family
    totals alone (no context), and of the raw finest cells (no back-off); calibration at DEFAULT_K."""
    out: dict = {}
    for name, (fit_obs, test_obs) in splits(obs).items():
        m = fit(fit_obs)
        # Knowing nothing: every outcome the family has, equally likely.
        uniform = sum(math.log(len(m["families"][o.family]["cells"].get("", {})) or 1) for o in test_obs)
        res = {"n_fit": len(fit_obs), "n_test": len(test_obs),
               "uniform": uniform / len(test_obs) if test_obs else float("nan"),
               "family_only": log_loss(m, test_obs, depth=0),
               "raw_cells": log_loss(m, test_obs, k=1e-9),
               "by_k": {str(k): log_loss(m, test_obs, k=k) for k in ks},
               "calibration": calibration(m, test_obs)}
        out[name] = res
    return out


# ---------------------------------------------------------------- report


FAMILY_TITLES = {
    "pf_unopened": "翻前：前面没人入池",
    "pf_vs_limp": "翻前：前面有人平跟",
    "pf_option": "翻前：不用补钱（大盲、补盲的人）",
    "pf_vs_raise": "翻前：面对一次加注（自己还没主动投钱）",
    "pf_limper_vs_raise": "翻前：平跟之后被加注",
    "pf_raiser_vs_3bet": "翻前：自己加注后被再加注（3bet）",
    "pf_other_vs_3bet": "翻前：面对 3bet（自己没加过注）",
    "pf_vs_4bet": "翻前：面对第三次及以后的加注",
    "po_lead": "翻后：轮到自己时还没人下注",
    "po_vs_bet": "翻后：面对第一次下注",
    "po_vs_raise": "翻后：面对加注",
}
DIM_TITLES = {
    "pos": "位置", "limpers": "平跟人数", "seat": "座位", "size": "面对的尺度（占底池）", "entered": "之前投过钱",
    "raised": "之前加过注", "role": "上一条街的主动方", "players": "人数", "street": "街", "was_bettor": "自己是下注的人",
}
VALUE_TITLES = {
    "field": "非盲注位", "aggr": "自己", "other": "别人", "none": "没有", "last": "最后行动", "early": "不是最后行动",
    "yes": "是", "no": "否", "flop": "翻牌", "turn": "转牌", "river": "河牌", "allin": "全下",
}
OUTCOME_TITLES = {"fold": "弃", "call": "跟", "raise": "加注", "check": "过牌", "bet": "下注"}
OUTCOME_ORDER = ("fold", "check", "call", "bet", "raise")
SPLIT_TITLES = {"hands": "随机一半牌局 → 另一半", "time": "前 60% 牌局 → 后 40%", "players": "一半玩家 → 另一半玩家"}


def combo_group(combo: str) -> str:
    """Coarse strength group of a 169-class label: strong (TT+, AQ+), pair, broadway, other."""
    ranks = "23456789TJQKA"
    a, b = ranks.index(combo[0]), ranks.index(combo[1])
    if a == b:
        return "strong" if a >= ranks.index("T") else "pair"
    if a == ranks.index("A") and b >= ranks.index("Q"):
        return "strong"
    if b >= ranks.index("T"):
        return "broadway"
    return "other"


def _interval(c: int, n: int) -> tuple[float, float]:
    """90% interval of a rate from c successes in n (Jeffreys prior)."""
    from .stats import beta_ppf

    return beta_ppf(0.05, c + 0.5, n - c + 0.5), beta_ppf(0.95, c + 0.5, n - c + 0.5)


def _val(v: str) -> str:
    return VALUE_TITLES.get(v, v)


_VALUE_ORDER = {v: i for i, v in enumerate(
    ("EP", "MP", "CO", "BTN", "SB", "BB", "field") + SIZE_LABELS + ("allin", "flop", "turn", "river",
                                                                    "aggr", "other", "none", "early", "last"))}


def _cell_order(cell: str) -> tuple:
    return tuple((_VALUE_ORDER.get(v, len(_VALUE_ORDER)), v) for v in cell.split("|"))


def render_markdown(model: dict, val: Optional[dict] = None) -> str:
    """Human-readable report (Chinese): hold-out checks first, then every family's finest cells."""
    import datetime

    out = ["# 牌池模型", ""]
    rng = model.get("ts_range")
    span = ""
    if rng:
        d0, d1 = (datetime.datetime.fromtimestamp(t, datetime.timezone.utc).strftime("%Y-%m-%d") for t in rng)
        span = f"，{d0} 至 {d1}"
    out.append(f"{model['hands']} 手、{model['players']} 个对手、{model['observations']} 个对手决策{span}。"
               f"细格往粗格收缩的强度 k = {model['k']:g}。尺度一律是占底池的比例。")
    out.append("")
    if val:
        out += ["## 样本外检验", "",
                "用一部分数据建模型，预测另一部分。中间四列是「平均给对的概率」= 给实际发生的那个动作打的概率的"
                "几何平均，越高越好：各动作均分（什么都不知道）、只用每类局面的总频率、本模型、不收缩的细格。"
                "「格子平均偏差」= 每个细格预测频率和实际频率之差的加权平均；「偏差 / 抽样误差」接近 1 表示偏差只是"
                "样本量造成的，明显大于 1 表示牌池在变。", "",
                "| 切分 | 建模 / 检验决策数 | 各动作均分 | 只用局面总频率 | 本模型 | 不收缩的细格 | 格子平均偏差 | 偏差 / 抽样误差 |",
                "|---|---|---|---|---|---|---|---|"]
        kkey = f"{model['k']:g}"
        for name, r in val.items():
            by_k = r["by_k"].get(kkey) or r["by_k"].get(str(float(model["k"])))
            c = r["calibration"]
            out.append(f"| {SPLIT_TITLES.get(name, name)} | {r['n_fit']} / {r['n_test']} | {math.exp(-r['uniform']):.3f} "
                       f"| {math.exp(-r['family_only']['_all']['loss']):.3f} | {math.exp(-by_k['_all']['loss']):.3f} "
                       f"| {math.exp(-r['raw_cells']['_all']['loss']):.3f} | {100 * c['mean_abs_gap']:.1f} 个点 "
                       f"| {c['rms_z']:.2f} |")
        out.append("")
    for fam, f in model["families"].items():
        root = f["cells"].get("", {})
        n_root = sum(root.values())
        if not n_root:
            continue
        outcomes = [a for a in OUTCOME_ORDER if a in root]
        out += [f"## {FAMILY_TITLES.get(fam, fam)}", "",
                f"共 {n_root} 次：" + "、".join(f"{OUTCOME_TITLES[a]} {100 * root[a] / n_root:.0f}%" for a in outcomes) + "。", ""]
        dims = f["dims"]
        head = [DIM_TITLES.get(d, d) for d in dims] + ["次数"] + [f"{OUTCOME_TITLES[a]}（90% 区间）" for a in outcomes]
        out += ["| " + " | ".join(head) + " |", "|" + "---|" * len(head)]
        for cell, cnt in sorted(f["cells"].items(), key=lambda kv: _cell_order(kv[0])):
            if not cell or cell.count("|") != len(dims) - 1:
                continue
            n = sum(cnt.values())
            cols = [_val(v) for v in cell.split("|")] + [str(n)]
            for a in outcomes:
                lo, hi = _interval(cnt.get(a, 0), n)
                cols.append(f"{100 * cnt.get(a, 0) / n:.0f}%（{100 * lo:.0f}–{100 * hi:.0f}）")
            out.append("| " + " | ".join(cols) + " |")
        out.append("")
        sizes = f["size_cells"].get("", {})
        n_sz = sum(sizes.values())
        if n_sz:
            labels = [x for x in model["size_labels"] if x in sizes]
            out.append(f"下注 / 加注的尺度（{n_sz} 次）：" + "、".join(f"{_val(x)} {100 * sizes[x] / n_sz:.0f}%" for x in labels) + "。")
            out.append("")
    if model.get("shown"):
        out += ["## 翻前各动作亮出来的牌", "",
                "只有打到摊牌才看得到牌，所以这是偏向「走得远的牌」的样本，弃牌那几行尤其不能当成范围。"
                "强牌 = TT+、AQ+；对子 = 22–99；两张高牌 = 两张都是 T 以上（不含强牌）。", "",
                "| 局面 | 动作 | 亮牌数 | 强牌 | 对子 | 两张高牌 | 其余 |", "|---|---|---|---|---|---|---|"]
        for key, combos in model["shown"].items():
            fam, outcome = key.split("|")
            n = sum(combos.values())
            g = Counter()
            for c, v in combos.items():
                g[combo_group(c)] += v
            out.append(f"| {FAMILY_TITLES.get(fam, fam)} | {OUTCOME_TITLES.get(outcome, outcome)} | {n} | "
                       + " | ".join(f"{100 * g[x] / n:.0f}%" for x in ("strong", "pair", "broadway", "other")) + " |")
        out.append("")
    return "\n".join(out)


def build(hands: Iterable[Hand], hero_names: Iterable[str] = (), k: float = DEFAULT_K, with_validation: bool = True
          ) -> tuple[dict, Optional[dict]]:
    """Model fitted on all hands, plus the hold-out results (fitted on parts of the same hands)."""
    obs = collect(hands, hero_names)
    ks = tuple(sorted({1.0, 2.0, 5.0, 10.0, 20.0, 50.0, float(k)}))
    return fit(obs, k), (validate(obs, ks) if with_validation else None)

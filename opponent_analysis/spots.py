"""Hand -> observations. One observation = one player, one opportunity, one outcome.

Every statistic is "times X happened / times the player could have done X"; the predicate
for "could have" is defined here and nowhere else. Decisions the server made for the
player (timeouts) are skipped.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Optional

from .cards import hand_class
from .model import STREETS, Hand
from .replay import position_class, ring_from


@dataclass
class Obs:
    name: str
    stat: str
    outcome: str
    dims: dict = field(default_factory=dict)
    # For single-spot EV formulas: pot before the bet and the bet, in big blinds.
    pot_bb: Optional[float] = None
    bet_bb: Optional[float] = None
    # Showdown percentile of the bettor's hand (river_bet_called only).
    pct: Optional[float] = None


def size_bucket(s: float) -> str:
    if s <= 0.4:
        return "<=0.4"
    if s <= 0.8:
        return "0.4-0.8"
    if s <= 1.2:
        return "0.8-1.2"
    return ">1.2"


def table_bucket(n: int) -> str:
    return "2-4" if n <= 4 else ("5-6" if n <= 6 else "7-9")


def depth_bucket(bb: float) -> str:
    if bb < 40:
        return "<40"
    if bb < 80:
        return "40-80"
    if bb <= 150:
        return "80-150"
    return ">150"


def _outcome3(kind: str) -> str:
    return {"fold": "fold", "call": "call", "check": "check"}.get(kind, "raise")


def hand_observations(h: Hand, river_percentile=None) -> list[Obs]:
    """All observations of one hand. `river_percentile(hole, board)` enables the showdown sample."""
    out: list[Obs] = []
    bb = h.bb
    name = h.names
    forced_seats = {d.seat for d in h.decisions if d.forced}
    pos = h.positions
    n = len(h.dealt_seats)
    stack_before = stacks_before(h)

    def eff_depth(seat: int) -> str:
        others = [h.start_stacks[s] for s in h.dealt_seats if s != seat]
        return depth_bucket(min(h.start_stacks[seat], max(others)) / bb)

    base = {s: {"table": table_bucket(n), "depth": eff_depth(s)} for s in h.dealt_seats}

    def emit(seat: int, stat: str, outcome: str, dims: Optional[dict] = None, **kw) -> None:
        nm = name.get(seat)
        if not nm:  # same test as report.Accumulator, which only creates players for non-empty names
            return
        d = dict(base[seat])
        if dims:
            d.update(dims)
        out.append(Obs(nm, stat, outcome, d, **kw))

    # ---------------- preflop, per hand
    pre = [d for d in h.decisions if d.street == "preflop"]
    first_raiser = next((d.seat for d in pre if d.kind == "raise"), None)
    seats_with_pre = []
    for d in pre:
        if d.seat not in seats_with_pre:
            seats_with_pre.append(d.seat)
    for s in seats_with_pre:
        if s in forced_seats:
            continue
        mine = [d for d in pre if d.seat == s]
        cls = position_class(pos[s])
        emit(s, "vpip", "yes" if any(d.kind in ("call", "raise") for d in mine) else "no", {"pos": cls})
        emit(s, "pfr", "yes" if any(d.kind == "raise" for d in mine) else "no", {"pos": cls})
        f = mine[0]
        # A first decision that is a check means the BB posted less than the SB: no pot to open.
        if f.raises_before == 0 and f.prior_callers == 0 and pos[s] not in ("BB", "BTN_BB") and f.kind != "check":
            emit(s, "rfi", {"raise": "raise", "call": "limp", "fold": "fold"}[f.kind], {"pos": pos[s]})
        if f.raises_before == 1 and f.aggressor is not None:
            o = _outcome3(f.kind)
            emit(s, "vs_open", o, {"pos": cls, "callers": "yes" if f.prior_callers else "no"})
            if (pos[s] in ("SB", "BB", "BTN_BB") and f.prior_callers == 0
                    and pos[f.aggressor] in ("CO", "BTN", "SB", "BTN_SB")):
                emit(s, "steal_defense", o, {"pos": pos[s]})
        if s == first_raiser:
            for d in mine:
                if d.raises_before == 2 and d.aggressor != s:
                    emit(s, "vs_3bet", _outcome3(d.kind), {"pos": cls})
                    break

    # ---------------- postflop
    last_aggr = {st: None for st in STREETS}
    last_aggr["preflop"] = h.preflop_aggressor
    for d in h.decisions:
        if d.kind in ("bet", "raise"):
            last_aggr[d.street] = d.seat

    live_at = {}
    for st in STREETS[1:]:
        live_at[st] = [s for s in h.dealt_seats if STREETS.index(h.streets_seen[s]) >= STREETS.index(st)
                       and s not in _folded_before(h, st)]

    acted: dict[tuple[str, int], list[str]] = {}
    for d in h.decisions:
        if d.street == "preflop":
            continue
        prev = acted.setdefault((d.street, d.seat), [])
        if d.forced:
            prev.append(d.kind)
            continue
        hu = "hu" if d.n_live == 2 else "mw"
        prev_st = STREETS[STREETS.index(d.street) - 1]
        prev_aggr = last_aggr[prev_st]
        order = [s for s in ring_from(h.dealt_seats, h.button_seat)[1:] + [h.button_seat] if s in live_at[d.street]]
        ip = "ip" if order and order[-1] == d.seat else "oop"
        role = "aggr" if prev_aggr == d.seat else ("none" if prev_aggr is None else "other")
        if d.raises_before == 0 and not prev:
            emit(d.seat, f"bet_first_{d.street}", "bet" if d.kind == "bet" else "check", {"hu": hu, "role": role})
            if d.street == "flop" and role == "aggr":
                emit(d.seat, "cbet", "bet" if d.kind == "bet" else "check", {"hu": hu, "ip": ip})
            if d.street == "flop" and role == "other" and prev_aggr in order and order.index(prev_aggr) > order.index(d.seat):
                emit(d.seat, "donk", "bet" if d.kind == "bet" else "check", {"hu": hu})
            if d.street in ("turn", "river") and role == "aggr":
                emit(d.seat, f"barrel_{d.street}", "bet" if d.kind == "bet" else "check", {"hu": hu})
        if d.raises_before == 1 and d.aggressor != d.seat and all(k == "check" for k in prev):
            bet = d.current_bet
            pot = d.street_start_pot
            s_ratio = bet / pot if pot else 0.0
            dims = {"hu": hu, "size": size_bucket(s_ratio), "after_check": "yes" if prev else "no"}
            clean = d.n_live == 2 and d.prior_callers == 0
            # Only what this player can match is at stake: a bet above his stack plays as his stack.
            # (He has put nothing in on this street yet: first bet, and he only checked before.)
            eff = min(bet, stack_before[d.idx])
            kw = {"pot_bb": pot / bb, "bet_bb": eff / bb} if clean else {}
            o = _outcome3(d.kind)
            emit(d.seat, f"vs_bet_{d.street}", o, dims, **kw)
            if d.street == "flop" and d.aggressor == h.preflop_aggressor:
                emit(d.seat, "vs_cbet", o, dims, **kw)
        if d.kind != "check":
            emit(d.seat, "postflop_aggr", "aggr" if d.kind in ("bet", "raise") else d.kind, {"street": d.street})
        prev.append(d.kind)

    # ---------------- showdown
    for s in h.dealt_seats:
        if s in forced_seats:
            continue
        if STREETS.index(h.streets_seen[s]) >= 1 and s not in _folded_before(h, "flop"):
            emit(s, "wtsd", "yes" if s in h.showdown_seats else "no")
    for s in h.showdown_seats:
        # Chips taken from the pot. Only the part of a seat's money that someone matched is at
        # stake; the unmatched rest is handed back and must not count as a win.
        matched = min(h.committed[s], max(h.committed[o] for o in h.dealt_seats if o != s))
        payout = h.final_stacks[s] - h.start_stacks[s] + matched
        emit(s, "wsd", "won" if payout > 0 else "lost")

    # Clean river sample: a single river bet into no prior bet, called by one player, then showdown.
    if river_percentile is not None and len(h.showdown_seats) == 2 and len(h.board) == 5:
        river = [d for d in h.decisions if d.street == "river"]
        aggr = [d for d in river if d.kind in ("bet", "raise")]
        if len(aggr) == 1 and aggr[0].raises_before == 0:
            b = aggr[0]
            after = [d for d in river if d.idx > b.idx]
            if len(after) == 1 and after[0].kind == "call" and b.seat in h.shown and not b.forced:
                pct = river_percentile(h.shown[b.seat], h.board)
                # A caller short of the bet calls all-in for less; the unmatched rest goes back.
                eff = min(b.delta, after[0].delta)
                emit(b.seat, "river_bet_called", "shown", {"size": size_bucket(b.delta / b.pot_before)},
                     pot_bb=b.pot_before / bb, bet_bb=eff / bb, pct=pct)
    return out


@dataclass
class PreflopAllin:
    """One voluntary preflop all-in, with the hole cards when the player showed them.

    Only shown hands reveal cards, and an uncalled jam is never shown, so the cards are a
    sample biased toward jams that got called.
    """

    hand_id: str
    ts: Optional[float]
    action: str  # "jam" = bet / raise all-in, "call" = call all-in
    facing: int  # raises before the action, capped at 3: 0 unopened, 1 open, 2 3bet, 3 4bet+
    callers: int  # calls since the last raise (unopened pot: limpers)
    pos: str
    n_live: int  # seats not folded before the action
    stack_bb: float  # his starting stack
    eff_bb: float  # min(his start, the largest start among the other seats still live then)
    cards: Optional[list[str]] = None
    combo: Optional[str] = None


def preflop_allins(h: Hand) -> list[tuple[str, PreflopAllin]]:
    """(player name, all-in) for every non-forced preflop decision that put the player all-in."""
    out = []
    folded: set[int] = set()
    for d in h.decisions:
        if d.street != "preflop":
            break
        if d.kind == "fold":
            folded.add(d.seat)
            continue
        nm = h.names.get(d.seat)
        if not d.all_in or d.forced or not nm:
            continue
        others = [h.start_stacks[s] for s in h.dealt_seats if s != d.seat and s not in folded]
        eff = min(h.start_stacks[d.seat], max(others)) if others else h.start_stacks[d.seat]
        cards = h.shown.get(d.seat)
        out.append((nm, PreflopAllin(
            hand_id=h.hand_id, ts=h.ts, action="call" if d.kind == "call" else "jam",
            facing=min(d.raises_before, 3), callers=d.prior_callers, pos=h.positions[d.seat], n_live=d.n_live,
            stack_bb=round(h.start_stacks[d.seat] / h.bb, 1), eff_bb=round(eff / h.bb, 1),
            cards=list(cards) if cards else None, combo=hand_class(cards) if cards else None,
        )))
    return out


def stacks_before(h: Hand) -> dict[int, int]:
    """Decision idx -> the acting seat's stack just before it (blinds posted as replay posts them)."""
    left = dict(h.start_stacks)
    for s, p in h.positions.items():
        blind = h.sb if p in ("SB", "BTN_SB") else (h.bb if p in ("BB", "BTN_BB") else 0)
        left[s] -= min(blind, left[s])
    out = {}
    for d in h.decisions:
        out[d.idx] = left[d.seat]
        left[d.seat] -= d.delta
    return out


def _folded_before(h: Hand, street: str) -> set[int]:
    idx = STREETS.index(street)
    return {d.seat for d in h.decisions if d.kind == "fold" and STREETS.index(d.street) < idx}

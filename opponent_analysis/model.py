"""Canonical hand record shared by all platform adapters.

Chip amounts stay integers in the platform's own units; conversion to big blinds happens
only in the statistics / report layers.
"""

from __future__ import annotations

from dataclasses import asdict, dataclass, field
from typing import Optional

STREETS = ("preflop", "flop", "turn", "river")


@dataclass
class RawAction:
    """One action as the adapter read it, before betting replay."""

    seat: int
    kind: str  # fold / check / call / bet / raise / all_in
    to_total: Optional[int] = None  # street total after a bet/raise, when the source gives it
    delta: Optional[int] = None  # chips put in by this action, when the source gives it
    stack_before: Optional[int] = None  # source's stack before the action (cross-check only)
    forced: Optional[bool] = None  # server acted for the player (timeout); None = unknown


@dataclass
class RawHand:
    platform: str
    hand_id: str
    table_id: str
    ts: Optional[float]
    button_seat: int
    sb: int
    bb: int
    dealt_seats: list[int]
    start_stacks: dict[int, int]
    names: dict[int, str]
    hero_seat: int
    hero_hole: list[str]
    board: list[str]
    actions: list[RawAction]
    final_stacks: dict[int, int]
    shown: dict[int, list[str]]
    # Net result per seat as reported by the source (final - start); None when not given.
    source_net: Optional[dict[int, int]] = None
    source_file: str = ""
    # Seats the source left out of final_stacks (left mid-hand); allowed only if they folded.
    missing_final: list[int] = field(default_factory=list)
    # Per-seat ante, posted by every dealt seat before the blinds: counts toward the pot and the
    # seat's total commitment but not toward the street's bet. 0 = no ante.
    ante: int = 0
    # Seats that post an extra live big blind at hand start (new / returning players on wopo):
    # counts as that seat's street bet, action order unchanged, may check when unraised.
    posts: list[int] = field(default_factory=list)


@dataclass
class Decision:
    """One replayed decision point plus the action taken there."""

    idx: int  # index in the hand's action list
    seat: int
    street: str
    kind: str  # normalized: fold / check / call / bet / raise
    all_in: bool
    delta: int
    to_total: int  # seat's street contribution after the action
    to_call: int  # chips needed to call before the action
    pot_before: int  # total pot (all streets, all seats) before the action
    street_start_pot: int
    current_bet: int  # highest street contribution before the action
    raises_before: int  # bets + raises already made on this street
    n_live: int  # seats not folded before the action (incl. all-in seats)
    n_can_act: int  # live seats not all-in before the action
    first_on_street: bool  # seat's first decision on this street
    aggressor: Optional[int]  # last bettor/raiser on this street before the action
    prior_callers: int  # calls since the last bet/raise on this street
    forced: Optional[bool]


@dataclass
class Hand:
    """Validated hand: raw facts + replayed decisions + derived seat info."""

    platform: str
    hand_id: str
    table_id: str
    ts: Optional[float]
    sb: int
    bb: int
    button_seat: int
    dealt_seats: list[int]
    positions: dict[int, str]
    start_stacks: dict[int, int]
    final_stacks: dict[int, int]
    names: dict[int, str]
    hero_seat: int
    board: list[str]
    shown: dict[int, list[str]]
    decisions: list[Decision]
    folded: list[int]
    showdown_seats: list[int]  # seats live at the end when two or more remained
    streets_seen: dict[int, str]  # last street each seat was live on
    preflop_aggressor: Optional[int]
    committed: dict[int, int]  # chips each seat put in over the hand (blinds included)
    source_file: str = ""
    notes: list[str] = field(default_factory=list)

    def to_json(self) -> dict:
        return asdict(self)

    @staticmethod
    def from_json(d: dict) -> "Hand":
        intkeys = ("positions", "start_stacks", "final_stacks", "names", "shown", "streets_seen", "committed")
        d = dict(d)
        for k in intkeys:
            d[k] = {int(s): v for s, v in d[k].items()}
        d["decisions"] = [Decision(**x) for x in d["decisions"]]
        return Hand(**d)

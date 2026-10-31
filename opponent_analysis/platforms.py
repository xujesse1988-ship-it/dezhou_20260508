"""Per-platform settings. Platforms are never pooled: every database, population prior and
report is per platform.

reveal_policy describes what the server shows at showdown:
  "all_live"   every hand still live at showdown is shown (losers cannot muck)
  "may_muck"   losing hands may be mucked; only voluntarily shown / winning hands are seen
Analyses that need an unbiased look at showdown hands (river bet bluff share) run only
under "all_live", and only when the data's measured reveal rate agrees (see report.py).
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class Platform:
    name: str
    heads_up_button_is_sb: bool
    reveal_policy: str
    forced_actions_in_hh: bool  # hh marks timeout actions itself (else needs the ws log)
    note: str


PLATFORMS = {
    "openpoker": Platform(
        name="openpoker",
        # Two-player hands: the reported button posts the big blind (seen in the hh:
        # the non-button seat posts 10 and acts first preflop).
        heads_up_button_is_sb=False,
        # Measured 2026-10-20 on 3,894 hh rows: 1,392 of 1,400 showdowns showed every live
        # hand; the 8 exceptions are logging gaps (missing folds), not mucks.
        reveal_policy="all_live",
        forced_actions_in_hh=False,
        note="hh lacks timeout flags; pass the ws log to mark forced actions",
    ),
    "poker_manager": Platform(
        name="poker_manager",
        heads_up_button_is_sb=True,  # docs/poker_manager_client.md: two-player BTN = SB
        # Measured 2026-10-20 on 1,022 hh rows: 61 of 61 showdowns showed every live hand.
        reveal_policy="all_live",
        forced_actions_in_hh=True,
        note="result.actions carries reason=timeout",
    ),
    "wopo": Platform(
        name="wopo",
        # The wopo driver only logs hands with 3+ dealt seats (two-player hands are skipped),
        # and the adapter rejects any that slip through, so this value is never used.
        heads_up_button_is_sb=True,
        # Measured 2026-10-29 on the two 2026-10-28 ante-table runs: hand_result.shown_cards held
        # 114 of 114 opponents live at showdown; the logging seat's own cards are never in it
        # (13 of 13). Kept at may_muck until the reveal-rate check leaves the hero seat out.
        reveal_policy="may_muck",
        forced_actions_in_hh=False,
        note="no timeout flag seen in player_action; shown_cards omits the hero seat",
    ),
}


def detect_platform(first_record: dict) -> str:
    if "poker_manager_hh" in first_record:
        return "poker_manager"
    if "wopo_hh" in first_record:
        return "wopo"
    if "hh" in first_record:
        return "openpoker"
    raise ValueError("unknown hand-history format")

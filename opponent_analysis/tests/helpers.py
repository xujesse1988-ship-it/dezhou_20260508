"""Builders for hand-written poker_manager-format records (the most complete source format)."""

from __future__ import annotations


def pm_record(hand_id, button, dealt, stacks, actions, final, board=(), shown=None, hero=None, names=None,
              sb=10, bb=20):
    """actions: list of (seat, action, delta) with delta = chips put in by the action (None for fold/check)."""
    names = names or {s: f"p{s}" for s in dealt}
    acts = []
    for seat, action, delta in actions:
        acts.append({"seat": seat, "name": names[seat], "action": action,
                     "contribution_delta": delta if delta is not None else 0})
    return {
        "poker_manager_hh": 1, "hand_id": hand_id, "table_id": "t", "complete": True,
        "button_seat": button, "dealt_seats": list(dealt), "small_blind": sb, "big_blind": bb,
        "initial_stacks": {str(s): v for s, v in stacks.items()},
        "my_seat": hero if hero is not None else dealt[0], "hole": [], "board": list(board),
        "result": {"final_stacks": {str(s): v for s, v in final.items()}, "actions": acts,
                   "shown_cards": {str(s): c for s, c in (shown or {}).items()}, "winners": []},
    }

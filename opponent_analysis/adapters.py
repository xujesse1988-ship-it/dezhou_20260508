"""Source format -> RawHand. One function per platform; no cross-platform defaults."""

from __future__ import annotations

import json
from datetime import datetime
from typing import Iterator, Optional

from .model import RawAction, RawHand
from .replay import ReplayError, assign_positions
from .platforms import PLATFORMS


def _intkeys(d: Optional[dict]) -> dict:
    return {int(k): v for k, v in (d or {}).items()}


# ---------------------------------------------------------------- OpenPoker

def openpoker_ws_actions(ws_path: str) -> dict[str, list[tuple[int, str, Optional[str]]]]:
    """hand_id -> [(seat, action, reason)] in server order, from an openpoker ws log."""
    out: dict[str, list] = {}
    seen: set[str] = set()
    with open(ws_path) as f:
        for line in f:
            o = json.loads(line)
            if o.get("dir") != "in":
                continue
            m = o.get("raw")
            if isinstance(m, str):
                try:
                    m = json.loads(m)
                except json.JSONDecodeError:
                    continue
            if not isinstance(m, dict) or m.get("type") != "player_action":
                continue
            # Reconnect replays can resend an event; action_id dedupes them.
            aid = m.get("action_id")
            if aid is not None:
                if aid in seen:
                    continue
                seen.add(aid)
            out.setdefault(m.get("hand_id"), []).append((m["seat"], m["action"], m.get("reason")))
    return out


def openpoker_raw(r: dict, source_file: str, ws: Optional[dict] = None) -> RawHand:
    plat = PLATFORMS["openpoker"]
    hr = r.get("hand_result")
    if not hr:
        raise ReplayError("no_result")
    final = _intkeys(hr.get("final_stacks"))
    if not final:
        raise ReplayError("no_final_stacks")
    # final_stacks omits a seat that left during the hand; its actions still prove it was dealt.
    dealt = sorted(set(final) | {a["seat"] for a in r["actions"]})
    _, sb_seat, bb_seat = assign_positions(dealt, r["button_seat"], plat.heads_up_button_is_sb)
    blind = {sb_seat: r["small_blind"], bb_seat: r["big_blind"]}
    acts = r["actions"]
    ext = r.get("actions_ext") or []
    if len(ext) < len(acts):
        raise ReplayError("ext_short", f"{len(ext)} < {len(acts)}")

    forced: list[Optional[bool]] = [None] * len(acts)
    if ws is not None and r["hand_id"] in ws:
        evs = ws[r["hand_id"]]
        if len(evs) == len(acts) and all(e[0] == a["seat"] and e[1] == a["action"] for e, a in zip(evs, acts)):
            forced = [e[2] == "timeout" for e in evs]

    net = {w["seat"]: w["amount"] for w in hr.get("winners") or [] if w.get("amount") is not None}
    start: dict[int, int] = {}
    for a, e in zip(acts, ext):
        s = a["seat"]
        if s not in start and e.get("stack_before") is not None:
            start[s] = e["stack_before"] + blind.get(s, 0)
    for s in dealt:
        if s not in start and s in final:
            # Never acted: only a BB walk; recover from the reported net win.
            start[s] = final[s] - net[s] if s in net else final[s] + blind.get(s, 0)

    raw_actions = []
    for i, (a, e) in enumerate(zip(acts, ext)):
        kind = a["action"]
        if kind == "allin":
            kind = "all_in"
        delta = e.get("contribution_delta")
        if kind == "all_in" and not delta:
            # Known driver defect: all-in logged without its amount; replay derives it from the stack.
            delta = None
        raw_actions.append(RawAction(
            seat=a["seat"], kind=kind, to_total=a.get("to") if kind in ("raise", "bet") else None,
            delta=delta if kind != "fold" else None, stack_before=e.get("stack_before"), forced=forced[i],
        ))
    return RawHand(
        platform="openpoker", hand_id=r["hand_id"], table_id=r.get("table_id", ""), ts=r.get("ts"),
        button_seat=r["button_seat"], sb=r["small_blind"], bb=r["big_blind"], dealt_seats=dealt,
        start_stacks=start, names={int(k): v for k, v in (r.get("names") or {}).items()},
        hero_seat=r["my_seat"], hero_hole=list(r.get("hole") or []), board=list(r.get("board") or []),
        actions=raw_actions, final_stacks=final, shown=_intkeys(hr.get("shown_cards")),
        source_net=net, source_file=source_file, missing_final=[s for s in dealt if s not in final],
    )


# ---------------------------------------------------------------- poker_manager

def _iso_ts(v: Optional[str]) -> Optional[float]:
    if not v:
        return None
    try:
        return datetime.fromisoformat(v).timestamp()
    except ValueError:
        return None


def poker_manager_raw(r: dict, source_file: str, ws: Optional[dict] = None) -> RawHand:
    res = r.get("result")
    if not res:
        raise ReplayError("no_result")
    if r.get("complete") is False:
        raise ReplayError("incomplete_flag")
    names: dict[int, str] = {}
    raw_actions = []
    for a in res.get("actions") or []:
        if a.get("name"):
            names[a["seat"]] = a["name"]
        kind = a["action"]
        if kind == "allin":
            kind = "all_in"
        raw_actions.append(RawAction(
            seat=a["seat"], kind=kind, to_total=None,
            delta=a.get("contribution_delta") if kind != "fold" else None,
            stack_before=a.get("stack_before"), forced=a.get("reason") == "timeout",
        ))
    for w in res.get("winners") or []:
        if w.get("name"):
            names.setdefault(w["seat"], w["name"])
    return RawHand(
        platform="poker_manager", hand_id=r["hand_id"], table_id=r.get("table_id", ""), ts=_iso_ts(res.get("ts")),
        button_seat=r["button_seat"], sb=r["small_blind"], bb=r["big_blind"], dealt_seats=list(r["dealt_seats"]),
        start_stacks=_intkeys(r.get("initial_stacks")), names=names, hero_seat=r["my_seat"],
        hero_hole=list(r.get("hole") or []), board=list(r.get("board") or []), actions=raw_actions,
        final_stacks=_intkeys(res.get("final_stacks")), shown=_intkeys(res.get("shown_cards")),
        source_net=None, source_file=source_file,
    )


ADAPTERS = {"openpoker": openpoker_raw, "poker_manager": poker_manager_raw}


def iter_records(path: str) -> Iterator[tuple[int, dict]]:
    with open(path) as f:
        for n, line in enumerate(f, 1):
            line = line.strip()
            if line:
                yield n, json.loads(line)

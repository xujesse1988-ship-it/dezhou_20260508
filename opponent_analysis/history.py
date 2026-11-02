"""Hand replay for finished wopo hands: hh log -> step-by-step table states, served over HTTP.

Source of truth is the driver's --hh-log (wopo_hh.jsonl, one line per finished hand, every
action/stack already cross-checked against the server by the driver). Next to it, when present:

- wopo_play_events.jsonl (--play-events): the hero's decision at each turn (advice, action
  probabilities, source, timing) and the time of every action;
- wopo_ranges.jsonl (advisor --range-log): opponents' estimated hand-strength mix per street.

Neither is required (runs before 2026-10-31 have only the hh log). Hands the driver did not log
(dead hands, ~8%) are not in the hh log and cannot be replayed; they are only counted.

Each hand is re-simulated from its start stacks (antes, blinds, extra posts, then each action's
contribution_delta) so every step carries every seat's stack / street bet / folded state and the
pot. The simulation is display only; the hand also goes through the same adapter + betting replay
as `ingest`, and a failure there (e.g. not_zero_sum on all-in pots with wopo's extra deductions)
is shown as a warning, the hand is still replayed.

    python3 -m opponent_analysis history --run 'artifacts/wopo*/' --port 8781
"""

from __future__ import annotations

import glob
import json
import os
import threading
from collections import Counter
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Optional
from urllib.parse import parse_qs, urlsplit

from .adapters import wopo_raw
from .cards import best_rank
from .live import range_event
from .model import STREETS
from .platforms import PLATFORMS
from .replay import ReplayError, assign_positions, replay

PAGE_PATH = os.path.join(os.path.dirname(__file__), "history.html")
HH_NAME = "wopo_hh.jsonl"
EVENTS_NAME = "wopo_play_events.jsonl"
RANGES_NAME = "wopo_ranges.jsonl"
BOARD_LEN = {"preflop": 0, "flop": 3, "turn": 4, "river": 5}
EVENT_TYPES = ("hand_start", "action", "hero_action", "hero_turn", "decision", "board", "dead", "result")


# ------------------------------------------------------------------ range strength backfill

# Range logs written before 2026-11-01 (562cca4) carry each seat's 1326 weights but no `strength`
# mix. range_strength recomputes it from the weights, following tools/openpoker_advisor.rs
# range_strength exactly (same classes, same rules); the advisor used the unrounded weights, the
# log holds them x1e6 rounded, so the result can differ in the 4th decimal.
STRENGTH_CLASSES = ("straight_flush", "quads", "full_house", "flush", "straight", "trips", "two_pair",
                    "top_pair", "pair", "nothing")
_RANK = {r: i for i, r in enumerate("23456789TJQKA")}
_class_cache: dict = {}


def _straight_high(ranks: set) -> Optional[int]:
    rs = set(ranks)
    if 12 in rs:
        rs.add(-1)  # wheel: A counts as 1
    for hi in range(12, 2, -1):
        if all(r in rs for r in range(hi - 4, hi + 1)):
            return hi
    return None


def hand_category(cards: list[str]) -> tuple[int, int]:
    """5-7 cards -> (category, pair rank). Category as src/eval.rs HandCategory: 0 high card,
    1 one pair, ..., 8 straight flush, 9 royal flush (A-high straight flush). Pair rank: the
    highest rank seen exactly twice (used for one pair), -1 if none."""
    ranks = [_RANK[c[0].upper()] for c in cards]
    suits: dict[str, list[int]] = {}
    for c, r in zip(cards, ranks):
        suits.setdefault(c[1].lower(), []).append(r)
    counts: dict[int, int] = {}
    for r in ranks:
        counts[r] = counts.get(r, 0) + 1
    pair = max((r for r, n in counts.items() if n == 2), default=-1)
    flush = next((rs for rs in suits.values() if len(rs) >= 5), None)
    if flush is not None:
        sf = _straight_high(set(flush))
        if sf is not None:
            return (9 if sf == 12 else 8), pair
    ns = sorted(counts.values(), reverse=True)
    if ns[0] == 4:
        return 7, pair
    if ns[0] == 3 and len(ns) > 1 and ns[1] >= 2:
        return 6, pair
    if flush is not None:
        return 5, pair
    if _straight_high(set(ranks)) is not None:
        return 4, pair
    if ns[0] == 3:
        return 3, pair
    if ns[0] == 2 and len(ns) > 1 and ns[1] == 2:
        return 2, pair
    if ns[0] == 2:
        return 1, pair
    return 0, pair


def _board_category(board: list[str]) -> int:
    if len(board) == 5:
        return hand_category(board)[0]
    counts: dict[int, int] = {}
    for c in board:
        counts[_RANK[c[0].upper()]] = counts.get(_RANK[c[0].upper()], 0) + 1
    ns = sorted(counts.values(), reverse=True)
    if ns[0] == 4:
        return 7
    if ns[0] == 3:
        return 3
    if ns[0] == 2:
        return 2 if len(ns) > 1 and ns[1] == 2 else 1
    return 0


def _combo_classes(combos: tuple, board: tuple, dead: tuple) -> list[int]:
    """Strength class index per combo (-1 = blocked by the board or the dead cards), cached per
    (board, dead): every seat of a solve, and every reload of the file, shares it."""
    key = (combos, board, dead)
    hit = _class_cache.get(key)
    if hit is not None:
        return hit
    if len(_class_cache) > 4096:
        _class_cache.clear()
    b = list(board)
    used = {c.lower() for c in board} | {c.lower() for c in dead}
    board_cat = _board_category(b)
    board_top = max(_RANK[c[0].upper()] for c in b)
    out = []
    for combo in combos:
        h = [combo[:2], combo[2:]]
        if h[0].lower() in used or h[1].lower() in used:
            out.append(-1)
            continue
        cat, pair = hand_category(h + b)
        if cat <= board_cat:
            out.append(9)  # nothing better than the board itself
        elif cat >= 8:
            out.append(0)
        elif cat == 1:
            out.append(7 if pair >= board_top else 8)
        else:
            out.append({7: 1, 6: 2, 5: 3, 4: 4, 3: 5, 2: 6, 0: 9}[cat])
    _class_cache[key] = out
    return out


def range_strength(weights: list, combos: tuple, board: list, dead: list) -> Optional[dict]:
    """One seat's weights -> {class: probability} (non-zero classes, 4 decimals), or None
    preflop / when every weighted combo is blocked."""
    if len(board) < 3 or len(weights) != len(combos):
        return None
    cls = _combo_classes(tuple(combos), tuple(board), tuple(dead))
    acc = [0.0] * len(STRENGTH_CLASSES)
    for w, k in zip(weights, cls):
        if k >= 0 and w > 0:
            acc[k] += w
    total = sum(acc)
    if total <= 0:
        return None
    return {name: round(a / total * 1e4) / 1e4 for name, a in zip(STRENGTH_CLASSES, acc) if a > 0}


def backfill_strength(rec: dict, combos: Optional[list]) -> bool:
    """Adds `strength` to the opponent seats of a range-log line that lacks it (older logs).
    Returns True when anything was filled. Lines that already have it are left alone."""
    if not combos or not isinstance(rec, dict) or not isinstance(rec.get("board"), list) or len(rec["board"]) < 3:
        return False
    filled = False
    for seat in rec.get("seats") or []:
        if not isinstance(seat, dict) or seat.get("hero") or isinstance(seat.get("strength"), dict):
            continue
        if not isinstance(seat.get("range"), list):
            continue
        st = range_strength(seat["range"], combos, rec["board"], list(rec.get("hole") or []))
        if st is not None:
            seat["strength"] = st
            filled = True
    return filled


# ------------------------------------------------------------------ one hand -> steps

def act_label(kind: str, to_call: int, delta: int, total: int, current_bet: int, all_in: bool) -> str:
    """Same wording as play.log: bet X (unraised street) / raise 到 X / call X, plus （全下）."""
    if kind == "fold":
        return "fold"
    if kind == "check" or (kind == "call" and delta == 0):
        return "check"
    if total > current_bet:
        s = f"bet {total}" if current_bet == 0 else f"raise 到 {total}"
    else:
        s = f"call {delta}"
    return s + ("（全下）" if all_in else "")


def _match_events(actions: list[dict], events: list[dict]) -> tuple[list[Optional[dict]], Optional[str]]:
    """i-th hh action <-> i-th action/hero_action event of the hand, while seat, action and chips
    agree. Returns (event or None per action, note when they stop agreeing)."""
    evs = [e for e in events if e.get("type") in ("action", "hero_action")]
    out: list[Optional[dict]] = [None] * len(actions)
    if not evs:
        return out, None
    for i, a in enumerate(actions):
        if i >= len(evs):
            return out, f"事件流只有 {len(evs)} 个动作，hh 有 {len(actions)} 个"
        e = evs[i]
        if (e.get("seat"), e.get("action"), e.get("delta") or 0) != (a.get("seat"), a.get("action"),
                                                                    a.get("contribution_delta") or 0):
            return out, f"第 {i + 1} 个动作和事件流对不上（hh 座{a.get('seat')} {a.get('action')}，" \
                        f"事件 座{e.get('seat')} {e.get('action')}），之后不显示建议和用时"
        out[i] = e
    return out, None


def _decision_view(events: list[dict], ev: dict) -> dict:
    """The hero's decision point for a hero_action event: the advice written for it plus the
    advisor's full decision record (also when it arrived too late, stale)."""
    turn = ev.get("turn")
    ht = next((e for e in events if e.get("type") == "hero_turn" and e.get("turn") == turn), None)
    dec = next((e for e in reversed(events) if e.get("type") == "decision" and e.get("turn") == turn), None)
    out = {"advice": ev.get("advice"), "match": ev.get("match"), "valid": (ht or dec or {}).get("valid"),
           "think_s": None, "decision": None}
    if ht is not None and isinstance(ht.get("ts"), (int, float)) and isinstance(ev.get("ts"), (int, float)):
        out["think_s"] = round(ev["ts"] - ht["ts"], 2)
    if dec is not None:
        out["decision"] = {k: dec.get(k) for k in ("action", "amount", "all_in", "advice", "probs", "chosen", "source",
                                                    "decide_ms", "stale", "stale_kind", "written", "rejected")}
    return out


def build_hand(rec: dict, events: Optional[list[dict]] = None, ranges: Optional[dict] = None,
               run: str = "", source: str = "") -> dict:
    """One hh record (+ its play events / range events) -> the replay the page shows."""
    events = events or []
    res = rec.get("result") or {}
    dealt = sorted(int(s) for s in rec["dealt_seats"])
    names = {int(k): v for k, v in (rec.get("names") or {}).items()}
    start = {int(k): v for k, v in (rec.get("initial_stacks") or {}).items()}
    final = {int(k): v for k, v in (res.get("final_stacks") or {}).items()}
    shown = {int(k): list(v) for k, v in (res.get("shown_cards") or {}).items()}
    hero = rec.get("my_seat")
    button, sbv, bbv, ante = rec["button_seat"], rec["small_blind"], rec["big_blind"], rec.get("ante") or 0
    posts = [int(s) for s in rec.get("posts") or []]
    board = list(rec.get("board") or [])
    notes: list[str] = list(rec.get("result_notes") or [])
    try:
        positions, sb_seat, bb_seat = assign_positions(dealt, button, PLATFORMS["wopo"].heads_up_button_is_sb)
    except ReplayError as e:
        positions, sb_seat, bb_seat = {}, None, None
        notes.append(f"位置算不出：{e}")

    stack = {s: start.get(s, 0) for s in dealt}
    bet = {s: 0 for s in dealt}
    committed = {s: 0 for s in dealt}
    folded: set[int] = set()
    last: dict[int, Optional[str]] = {s: None for s in dealt}

    def put(s: int, amount: int) -> int:
        amt = min(amount, stack[s])
        stack[s] -= amt
        committed[s] += amt
        return amt

    for s in dealt:
        put(s, ante)  # dead money: in the pot, not this street's bet
    blinds = [(sb_seat, sbv, "SB"), (bb_seat, bbv, "BB")] + [(s, bbv, "补盲") for s in posts]
    for s, amount, tag in blinds:
        if s in stack:
            bet[s] += put(s, amount)
            last[s] = f"{tag} {bet[s]}"

    def snap(kind: str, street: str, shown_board: list, **kw) -> dict:
        return {"kind": kind, "street": street, "board": list(shown_board), "pot": sum(committed.values()),
                "seats": [[stack[s], bet[s], s in folded, stack[s] == 0 and s not in folded, last[s]] for s in dealt],
                **kw}

    street = "preflop"
    steps = [snap("start", street, [], label=f"开局：ante {ante}，盲注 {sbv}/{bbv}" if ante else f"开局：盲注 {sbv}/{bbv}")]
    actions = rec.get("actions") or []
    matched, match_note = _match_events(actions, events)
    if match_note:
        notes.append(match_note)
    prev_ts = next((e["ts"] for e in events if e.get("type") == "hand_start" and isinstance(e.get("ts"), (int, float))),
                   None)
    board_ts = {e.get("street"): e.get("ts") for e in events if e.get("type") == "board"}

    def deal_to(target: str) -> None:
        nonlocal street, prev_ts
        while STREETS.index(street) < STREETS.index(target):
            street = STREETS[STREETS.index(street) + 1]
            for s in dealt:
                bet[s] = 0
                if s not in folded:
                    last[s] = None
            cards = board[:BOARD_LEN[street]]
            if len(cards) < BOARD_LEN[street]:
                notes.append(f"公共牌不全：{street} 只有 {len(cards)} 张")
            new = cards[BOARD_LEN[STREETS[STREETS.index(street) - 1]]:]
            if isinstance(board_ts.get(street), (int, float)):
                prev_ts = board_ts[street]
            steps.append(snap("board", street, cards, label=f"{STREET_CN[street]} {' '.join(new)}", new=new))

    for i, a in enumerate(actions):
        s = a["seat"]
        if a.get("street") in STREETS and STREETS.index(a["street"]) > STREETS.index(street):
            deal_to(a["street"])
        if s not in stack:
            notes.append(f"第 {i + 1} 个动作的座位 {s} 不在发牌座位里")
            continue
        if a.get("stack_before") is not None and a["stack_before"] != stack[s]:
            notes.append(f"第 {i + 1} 个动作 座{s} 记录的剩余栈 {a['stack_before']}，重放 {stack[s]}")
        kind = a["action"]
        current = max(bet.values())
        to_call = max(0, current - bet[s])
        delta = 0 if kind == "fold" else int(a.get("contribution_delta") or 0)
        put(s, delta)
        bet[s] += delta
        if kind == "fold":
            folded.add(s)
        label = act_label(kind, to_call, delta, bet[s], current, delta > 0 and stack[s] == 0)
        last[s] = label
        step = snap("action", street, board[:BOARD_LEN[street]], seat=s, action=kind, label=label, to_call=to_call,
                    delta=delta, total=bet[s])
        ev = matched[i]
        if ev is not None and isinstance(ev.get("ts"), (int, float)):
            step["ts"] = ev["ts"]
            if prev_ts is not None:
                step["think_s"] = round(ev["ts"] - prev_ts, 2)
            prev_ts = ev["ts"]
        if ev is not None and ev.get("type") == "hero_action":
            step["hero"] = _decision_view(events, ev)
        steps.append(step)

    live = [s for s in dealt if s not in folded]
    if len(live) >= 2 and len(board) > BOARD_LEN[street]:
        deal_to(STREETS[min(3, max(i for i, st in enumerate(STREETS) if BOARD_LEN[st] <= len(board)))])

    # Result: final stacks as the server reported them (before wopo's rake, which comes off later).
    for s in dealt:
        bet[s] = 0
        if s in final:
            stack[s] = final[s]
    payouts = {int(p["seat"]): p.get("amount") for p in res.get("payouts") or [] if p.get("seat") is not None}
    for s in dealt:
        if s in payouts:
            last[s] = f"收 {payouts[s]}"
        elif s in live and len(live) >= 2:
            last[s] = "输"
    put_in = sum(committed.values())
    total_pot = res.get("total_pot")
    steps.append(snap("result", street, board[:max(BOARD_LEN[street], len(board) if len(live) >= 2 else 0)],
                      label="结算", pot=total_pot if total_pot is not None else put_in,
                      returned=put_in - total_pot if isinstance(total_pot, int) and put_in > total_pot else 0))

    seats = []
    showdown = len(live) >= 2
    for s in dealt:
        hole = list(rec.get("hole") or []) if s == hero else shown.get(s)
        # Made hand at showdown (0 high card .. 8 straight flush), for seats whose cards are known.
        made = None
        if showdown and s in live and hole and len(hole) == 2 and len(board) == 5:
            try:
                made = best_rank(hole + board)[0]
            except (ValueError, IndexError):
                notes.append(f"座{s} 的牌算不出牌型：{hole} {board}")
        seats.append({"seat": s, "name": names.get(s), "pos": positions.get(s), "hero": s == hero, "start": start.get(s),
                      "final": final.get(s), "net": (final[s] - start[s]) if s in final and s in start else None,
                      "hole": hole or None, "shown": s in shown, "committed": committed[s],
                      "showdown": s in live and showdown, "made": made})

    check = None
    try:
        replay(wopo_raw(rec, source), PLATFORMS["wopo"].heads_up_button_is_sb)
    except ReplayError as e:
        check = {"code": e.code, "detail": e.detail}
    except (KeyError, TypeError, ValueError) as e:
        check = {"code": "malformed", "detail": f"{type(e).__name__}: {e}"}

    hand_start = next((e for e in events if e.get("type") == "hand_start"), {})
    n_table = max([int(hand_start.get("n_seats") or 0), max(dealt) + 1, len(dealt)])
    return {
        "hand_id": rec["hand_id"], "table_id": rec.get("table_id"), "ts": rec.get("ts"), "run": run, "source": source,
        "button": button, "sb_seat": sb_seat, "bb_seat": bb_seat, "sb": sbv, "bb": bbv, "ante": ante, "posts": posts,
        "hero_seat": hero, "n_table": n_table, "board": board, "seats": seats, "steps": steps,
        "winners": res.get("winners") or [], "payouts": res.get("payouts") or [], "total_pot": res.get("total_pot"),
        "outcome_kind": res.get("outcome_kind"), "ranges": ranges or {}, "notes": notes, "check": check,
        "has_events": bool(events),
    }


STREET_CN = {"preflop": "翻前", "flop": "翻牌", "turn": "转牌", "river": "河牌"}


def summarize(h: dict) -> dict:
    """The hand list row: enough to filter and sort without loading the steps."""
    hero = next((x for x in h["seats"] if x["hero"]), None)
    hero_steps = [st for st in h["steps"] if st["kind"] == "action" and st.get("seat") == h["hero_seat"]]
    vpip = any(st["street"] == "preflop" and st["action"] in ("call", "raise", "all_in") and st["delta"] > 0
               for st in hero_steps)
    pfr = any(st["street"] == "preflop" and st["total"] > 0 and st["label"].startswith(("raise", "bet"))
              for st in hero_steps)
    decided = [st["hero"] for st in hero_steps if st.get("hero")]
    # Biggest winner's net: the server's winners[].amount (final - start); the seats' own
    # final - start only as a fallback, it is wrong when a stack was reset mid-hand (hand 379).
    wins = [w["amount"] for w in h["winners"] if isinstance(w.get("amount"), int)]
    nets = [x["net"] for x in h["seats"] if x["net"] is not None]
    win_max = max(wins) if wins else max(nets, default=None)
    # Made hands of the seats that won at showdown (cards known: shown, or our own).
    made = {x["seat"]: x["made"] for x in h["seats"] if x["made"] is not None}
    win_made = sorted({made[w["seat"]] for w in h["winners"] if w.get("seat") in made}, reverse=True)
    last_action_street = max((STREETS.index(st["street"]) for st in h["steps"] if st["kind"] == "action"), default=0)
    return {
        "key": h["key"], "hand_id": h["hand_id"], "run": h["run"], "ts": h["ts"], "table_id": h["table_id"],
        "sb": h["sb"], "bb": h["bb"], "ante": h["ante"], "n": len(h["seats"]),
        "hero_pos": hero and hero["pos"], "hole": hero and hero["hole"], "board": h["board"],
        "hero_net": hero and hero["net"], "hero_vpip": vpip, "hero_pfr": pfr,
        "hero_folded_pre": bool(hero_steps) and hero_steps[0]["action"] == "fold" and hero_steps[0]["street"] == "preflop",
        "showdown": any(x["showdown"] for x in h["seats"]), "shown": sum(1 for x in h["seats"] if x["shown"]),
        "street": STREETS[last_action_street], "pot": h["total_pot"], "win_max": win_max,
        "win_made": win_made, "has_ranges": bool(h["ranges"]),
        "decisions": len(decided), "diff": sum(1 for d in decided if d.get("match") == "diff"),
        "no_advice": sum(1 for d in decided if d.get("match") is None),
        "players": [x["name"] for x in h["seats"] if not x["hero"]],
        "check": h["check"]["code"] if h["check"] else None, "notes": len(h["notes"]), "has_events": h["has_events"],
    }


# ------------------------------------------------------------------ files

def expand(patterns: list[str]) -> list[str]:
    """Run directories, hh files or globs of either -> existing hh files (sorted, deduped)."""
    out: list[str] = []
    for p in patterns:
        hits = sorted(glob.glob(p)) or ([p] if os.path.exists(p) else [])
        for m in hits:
            f = os.path.join(m, HH_NAME) if os.path.isdir(m) else m
            if os.path.isfile(f):
                out.append(os.path.normpath(f))
    return list(dict.fromkeys(out))


def _sig(path: str):
    try:
        st = os.stat(path)
        return (st.st_size, st.st_mtime_ns)
    except OSError:
        return None


def _read_jsonl(path: str, counts: Counter, tag: str):
    try:
        f = open(path, encoding="utf-8")
    except OSError:
        return
    with f:
        for line in f:
            if not line.endswith("\n"):  # still being written
                break
            line = line.strip()
            if not line:
                continue
            try:
                yield json.loads(line)
            except json.JSONDecodeError:
                counts[f"{tag}_bad_line"] += 1


def load_run(hh_path: str) -> dict:
    """One hh file + its companions -> {hands: [replay], dead: [hand_id], counts, run}."""
    d = os.path.dirname(hh_path)
    run = os.path.basename(d) or d
    counts: Counter = Counter()
    events: dict[str, list] = {}
    for e in _read_jsonl(os.path.join(d, EVENTS_NAME), counts, "events"):
        if isinstance(e, dict) and e.get("type") in EVENT_TYPES and e.get("hand_id") is not None:
            events.setdefault(str(e["hand_id"]), []).append(e)
    ranges: dict[str, dict] = {}
    combos = None
    for rec in _read_jsonl(os.path.join(d, RANGES_NAME), counts, "ranges"):
        if isinstance(rec, dict) and isinstance(rec.get("combos"), list):  # header: combo order
            combos = rec["combos"]
            continue
        filled = backfill_strength(rec, combos)
        ev = range_event(rec)
        if ev is not None:  # the latest solve of a street wins, as on the live page
            counts["ranges_backfilled"] += int(filled)
            ranges.setdefault(str(ev["hand_id"]), {})[ev.get("street")] = {
                **{k: ev.get(k) for k in ("street", "board", "path", "range_src", "source", "seats")}, "backfill": filled}
    hands, seen = [], set()
    for rec in _read_jsonl(hh_path, counts, "hh"):
        if not isinstance(rec, dict) or "wopo_hh" not in rec:
            counts["not_wopo"] += 1
            continue
        hid = str(rec.get("hand_id"))
        if hid in seen:
            counts["duplicate"] += 1
            continue
        seen.add(hid)
        try:
            hands.append(build_hand(rec, events.get(hid), ranges.get(hid), run, hh_path))
        except Exception as e:  # one bad line must not hide the rest of the run
            counts["build_error"] += 1
            hands.append({"hand_id": hid, "run": run, "source": hh_path, "ts": rec.get("ts"),
                          "error": f"{type(e).__name__}: {e}"})
    dead = [hid for hid, evs in events.items() if hid not in seen and any(e.get("type") == "hand_start" for e in evs)]
    counts["hands"] = len(hands)
    st = _sig(hh_path)
    return {"run": run, "path": hh_path, "mtime": st[1] / 1e9 if st else None, "hands": hands, "dead": dead, "counts": dict(counts),
            "events": os.path.exists(os.path.join(d, EVENTS_NAME)), "ranges": os.path.exists(os.path.join(d, RANGES_NAME))}


class History:
    """The hh files matching `patterns` (re-expanded on every refresh, so a live session's new run
    directory shows up), each reloaded when it or its companion files change."""

    def __init__(self, patterns: list[str]):
        self.patterns = list(patterns)
        self.lock = threading.Lock()
        self.runs: dict[str, tuple] = {}  # hh path -> (sigs, run)
        self.by_key: dict[str, dict] = {}
        self.listing: dict = {}

    def refresh(self) -> None:
        with self.lock:
            changed = False
            paths = expand(self.patterns)
            for p in list(self.runs):
                if p not in paths:
                    del self.runs[p]
                    changed = True
            for p in paths:
                d = os.path.dirname(p)
                sigs = tuple(_sig(x) for x in (p, os.path.join(d, EVENTS_NAME), os.path.join(d, RANGES_NAME)))
                old = self.runs.get(p)
                if old is None or old[0] != sigs:
                    self.runs[p] = (sigs, load_run(p))
                    changed = True
            if changed or not self.listing:
                self._index()

    def _index(self) -> None:
        by_key: dict[str, dict] = {}
        rows, runs, errors = [], [], []
        for p in sorted(self.runs):
            run = self.runs[p][1]
            runs.append({k: run[k] for k in ("run", "path", "mtime", "counts", "events", "ranges")} | {"dead": len(run["dead"])})
            for h in run["hands"]:
                key = h["hand_id"] if h["hand_id"] not in by_key else f"{h['hand_id']}@{run['run']}"
                h["key"] = key
                by_key[key] = h
                if "error" in h:
                    errors.append({"key": key, "run": h["run"], "error": h["error"]})
                else:
                    rows.append(summarize(h))
        rows.sort(key=lambda r: (r["ts"] or "", r["hand_id"]), reverse=True)
        runs.sort(key=lambda r: (r["mtime"] or 0, r["path"]), reverse=True)  # newest hh file first
        self.by_key = by_key
        self.listing = {"hands": rows, "runs": runs, "errors": errors, "patterns": self.patterns}

    def hands(self) -> dict:
        self.refresh()
        with self.lock:
            return self.listing

    def hand(self, key: str) -> Optional[dict]:
        self.refresh()
        with self.lock:
            return self.by_key.get(key)


# ------------------------------------------------------------------ HTTP

def handle(hist: History, path: str, query: str, live: bool):
    """Shared by the standalone server and `live`: (status, body bytes, content type) or None."""
    if path == "/history" or (path == "/" and not live):
        with open(PAGE_PATH, "rb") as f:
            return 200, f.read(), "text/html; charset=utf-8"
    if path == "/api/history/hands":
        body = dict(hist.hands(), live=live)
        return 200, json.dumps(body, ensure_ascii=False).encode(), "application/json; charset=utf-8"
    if path == "/api/history/hand":
        key = parse_qs(query).get("key", [""])[0]
        h = hist.hand(key)
        if h is None:
            return 404, json.dumps({"error": f"没有这一手：{key}"}, ensure_ascii=False).encode(), \
                "application/json; charset=utf-8"
        return 200, json.dumps(h, ensure_ascii=False).encode(), "application/json; charset=utf-8"
    return None


def serve(patterns: list[str], host: str, port: int) -> None:
    from .live import GZIP_MIN_BYTES, accepts_gzip
    import gzip

    hist = History(patterns)
    hist.refresh()
    n = len(hist.listing.get("hands", []))
    files = [r["path"] for r in hist.listing.get("runs", [])]
    if not files:
        raise SystemExit(f"没有找到 {HH_NAME}：{' '.join(patterns)}")

    class H(BaseHTTPRequestHandler):
        def log_message(self, fmt, *args):
            pass

        def do_GET(self):
            u = urlsplit(self.path)
            r = handle(hist, u.path, u.query, live=False)
            code, body, ctype = r if r is not None else (404, b"not found", "text/plain; charset=utf-8")
            gz = len(body) >= GZIP_MIN_BYTES and accepts_gzip(self.headers.get("Accept-Encoding"))
            if gz:
                body = gzip.compress(body, compresslevel=6)
            self.send_response(code)
            self.send_header("Content-Type", ctype)
            if gz:
                self.send_header("Content-Encoding", "gzip")
            self.send_header("Vary", "Accept-Encoding")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(body)

    httpd = ThreadingHTTPServer((host, port), H)
    print(f"{n} hands from {len(files)} file(s): {', '.join(files)}", flush=True)
    print(f"serving http://{host}:{port}/", flush=True)
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        httpd.server_close()

"""Live opponent profiles: follow growing hand-history logs and serve the profiles over HTTP.

Only hand-history log files are read (the same *_hh.jsonl the drivers write, one line per
finished hand). Each new line goes through the same adapter + replay validation as
`ingest` and is appended to the platform database, so a restart resumes from the database
and the followed files are re-read with hand_id dedupe.

Optionally (--play-events) the wopo driver's play-event stream is followed too: one JSON line
per play.log line, written while the hand is played. It only feeds the "action guide" panel
(/api/play): the current hand and the last N hands, held in memory, never written to the
database.

    python3 -m opponent_analysis live --db artifacts/opponent_db --platform poker_manager \
        --follow 'artifacts/poker_manager_live_*/poker_manager_hh.jsonl' --port 8780
"""

from __future__ import annotations

import glob
import json
import math
import os
import threading
import time
import traceback
from collections import Counter, OrderedDict, deque
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Optional
from urllib.parse import parse_qs, urlsplit

from . import db
from .cards import river_percentile
from .model import Hand
from .platforms import detect_platform
from .report import (KEY_RATES, LEAK_NAMES, MIX_WARN, Accumulator, Config, _tag, allin_json, profile_json, render_markdown,
                     trend)
from .stats import Posterior, posterior

PAGE_PATH = os.path.join(os.path.dirname(__file__), "live.html")


# ------------------------------------------------------------------ log following

class Follower:
    """Reads complete new lines from every file matching the glob patterns.

    Patterns are re-expanded on each poll, so a new run directory is picked up. A file that
    shrinks or is replaced (new inode) is read again from the start; dedupe is by hand_id.
    A trailing line without its newline is left for the next poll (the writer is mid-line).

    tail_bytes: a file seen for the first time is read from its last tail_bytes only (the
    partial first line is skipped); None = from the start. A replaced / shrunk file is still
    read from the start.
    """

    def __init__(self, patterns: list[str], tail_bytes: Optional[int] = None):
        self.patterns = patterns
        self.tail_bytes = tail_bytes
        self.files: dict[str, dict] = {}

    def poll(self) -> list[tuple[str, int, dict]]:
        out = []
        paths = sorted({p for pat in self.patterns for p in glob.glob(pat)})
        for path in paths:
            try:
                st = os.stat(path)
            except FileNotFoundError:
                continue
            f = self.files.get(path)
            if f is None or f["ino"] != st.st_ino or st.st_size < f["offset"]:
                start = 0
                if f is None and self.tail_bytes is not None and st.st_size > self.tail_bytes:
                    start = st.st_size - self.tail_bytes
                f = self.files[path] = {"ino": st.st_ino, "offset": start, "lines": 0, "bad_lines": 0,
                                        "mtime": None, "skip_partial": start > 0}
            f["mtime"] = st.st_mtime
            if st.st_size == f["offset"]:
                continue
            with open(path, "rb") as fh:
                fh.seek(f["offset"])
                data = fh.read(st.st_size - f["offset"])
            if f["skip_partial"]:
                # Started mid-file: drop everything up to the first newline (a partial line),
                # unless the byte before the start is itself a newline.
                with open(path, "rb") as fh:
                    fh.seek(f["offset"] - 1)
                    at_line_start = fh.read(1) == b"\n"
                if not at_line_start:
                    cut = data.find(b"\n")
                    if cut < 0:
                        continue
                    f["offset"] += cut + 1
                    data = data[cut + 1:]
                f["skip_partial"] = False
            end = data.rfind(b"\n")
            if end < 0:
                continue
            f["offset"] += end + 1
            for raw in data[: end + 1].splitlines():
                f["lines"] += 1
                if not raw.strip():
                    continue
                try:
                    out.append((path, f["lines"], json.loads(raw)))
                except json.JSONDecodeError:
                    f["bad_lines"] += 1
        return out


class PctCache:
    """river_percentile memo persisted next to the database (it costs ~0.1-0.2 s per call)."""

    def __init__(self, path: str):
        self.path = path
        self.d: dict[str, float] = {}
        self.dirty = False
        if os.path.exists(path):
            with open(path) as f:
                self.d = json.load(f)

    def __call__(self, hole: list[str], board: list[str]) -> float:
        key = " ".join(sorted(c.lower() for c in hole)) + "|" + " ".join(sorted(c.lower() for c in board))
        v = self.d.get(key)
        if v is None:
            v = self.d[key] = river_percentile(hole, board)
            self.dirty = True
        return v

    def save(self) -> None:
        if not self.dirty:
            return
        tmp = self.path + ".tmp"
        with open(tmp, "w") as f:
            json.dump(self.d, f)
        os.replace(tmp, self.path)
        self.dirty = False


# ------------------------------------------------------------------ live state

class Live:
    def __init__(self, db_dir: str, platform: str, patterns: list[str], cfg: Config, refresh_s: float = 5.0,
                 hero_names=()):
        self.hero_extra = tuple(hero_names)
        self.db_dir = db_dir
        self.platform = platform
        self.cfg = cfg
        self.refresh_s = refresh_s
        self.store = db.Store(db_dir, platform)
        self.follower = Follower(patterns)
        self.pct = PctCache(os.path.join(self.store.dir, "river_pct_cache.json"))
        self.rejected = [{"code": r["code"]} for r in db.load_rejected(db_dir, platform)]
        self.counts: Counter = Counter()  # this process: what happened to followed lines
        self.errors: deque = deque(maxlen=20)
        self.recent: deque = deque(maxlen=40)
        self.tables: dict[str, dict] = {}
        self.acc = Accumulator(platform, self.pct, self.hero_extra)
        self.started = time.time()
        self.version = 0
        self.snapshot: dict = {}
        self.res: Optional[dict] = None
        self.md = ""
        self.profile: dict = {}
        self.dirty = True
        self.last_build = 0.0
        self.lock = threading.Lock()

    # -- hands -> accumulator

    def _feed(self, h: Hand) -> None:
        self.acc.add(h)
        t = self.tables.get(h.table_id)
        if t is None or (h.ts or 0) >= (t["ts"] or 0):
            self.tables[h.table_id] = {
                "table_id": h.table_id, "hand_id": h.hand_id, "ts": h.ts, "bb": h.bb,
                "seats": [{"seat": s, "name": h.names.get(s), "pos": h.positions[s],
                           "stack_bb": round(h.final_stacks[s] / h.bb, 1)}
                          for s in h.dealt_seats],
            }
        self.recent.appendleft(_hand_line(h))

    def _rebuild(self) -> None:
        self.acc = Accumulator(self.platform, self.pct, self.hero_extra)
        self.tables.clear()
        self.recent.clear()
        for h in db.load_hands(self.db_dir, self.platform):
            self._feed(h)

    def bootstrap(self) -> None:
        self._rebuild()
        self.step()
        self.build(force=True)

    def step(self) -> int:
        new: list[Hand] = []
        try:
            for path, line_no, rec in self.follower.poll():
                try:
                    plat = detect_platform(rec)
                except (ValueError, TypeError):
                    self.counts["unknown_format"] += 1
                    continue
                if plat != self.platform:
                    self.counts["other_platform"] += 1  # platforms are never mixed
                    continue
                key, hand = self.store.add(rec, path, None, line_no)
                self.counts[key] += 1
                if key.startswith("rejected:"):
                    self.rejected.append({"code": key.split(":", 1)[1]})
                    hid = rec.get("hand_id") if isinstance(rec, dict) else None
                    self.errors.appendleft({"ts": time.time(), "msg": f"{hid} {key} ({path}:{line_no})"})
                if hand is not None:
                    new.append(hand)
        finally:
            # Hands already in the database must reach the profile even when a later line
            # raised (e.g. a write error); otherwise the page would silently lag the database.
            self._absorb(new)
        return len(new)

    def _absorb(self, new: list[Hand]) -> None:
        if not new:
            return
        new.sort(key=lambda h: (h.ts is None, h.ts or 0.0))
        last = self.acc.last_ts
        if last is not None and new[0].ts is not None and new[0].ts < last:
            # An older file showed up: drift splits by arrival order, so rebuild in time order.
            self._rebuild()
        else:
            for h in new:
                self._feed(h)
        self.dirty = True
        self.store.log({"source": "live", "counts": {"accepted": len(new)}})

    def build(self, force: bool = False) -> None:
        if not (self.dirty or force):
            return
        res = self.acc.finalize(self.rejected, self.cfg)
        snap = snapshot_json(res, self)
        md = render_markdown(res)
        prof = profile_json(res)
        self.pct.save()
        with self.lock:
            self.version += 1
            snap["version"] = self.version
            self.snapshot = snap
            self.res = res
            self.md = md
            self.profile = prof
        self.dirty = False
        self.last_build = time.time()

    def close(self) -> None:
        self.store.close()

    def run(self, stop: threading.Event, poll_s: float) -> None:
        while not stop.is_set():
            try:
                self.step()
                if self.dirty and time.time() - self.last_build >= self.refresh_s:
                    self.build()
            except Exception as e:  # keep serving the last good snapshot
                self.errors.appendleft({"ts": time.time(), "msg": f"{type(e).__name__}: {e}"})
                traceback.print_exc()
            stop.wait(poll_s)


def _hand_line(h: Hand) -> dict:
    pot = sum(h.committed.values())
    return {
        "hand_id": h.hand_id, "table_id": h.table_id, "ts": h.ts, "n": len(h.dealt_seats),
        "pot_bb": round(pot / h.bb, 1), "board": h.board,
        "winners": [h.names.get(s) for s in h.dealt_seats if h.final_stacks[s] > h.start_stacks[s]],
        "shown": {h.names.get(s, str(s)): c for s, c in h.shown.items()},
        "hero_net_bb": (round((h.final_stacks[h.hero_seat] - h.start_stacks[h.hero_seat]) / h.bb, 1)
                        if h.hero_seat in h.final_stacks else None),
    }


# ------------------------------------------------------------------ action guide (wopo play events)

class PlayFeed:
    """Follows the wopo driver's --play-events JSONL and keeps the newest `max_hands` hands.

    Events are grouped by hand_id in arrival order; hands are ordered by their first event's
    ts (so several followed files interleave correctly). A file seen for the first time is read
    from its last `tail_bytes` only, which is plenty for max_hands hands, so a restart recovers
    the recent hands without reading a long session. Identical re-read events are dropped; an
    event for a hand already pushed out of the window is ignored (it would otherwise come back
    as the newest hand).
    """

    def __init__(self, patterns: list[str], max_hands: int = 20, tail_bytes: int = 8 << 20):
        self.patterns = patterns
        self.max_hands = max(1, max_hands)
        self.follower = Follower(patterns, tail_bytes=tail_bytes)
        self.hands: dict[str, dict] = {}
        self.dropped: OrderedDict = OrderedDict()
        self.seq = 0
        self.counts: Counter = Counter()
        self.errors: deque = deque(maxlen=20)
        self.version = 0
        self.lock = threading.Lock()
        self.body: dict = {}
        self._publish()

    def step(self) -> int:
        """Reads new events; returns how many were added (0 = no change, version unchanged)."""
        added = 0
        for path, line_no, rec in self.follower.poll():
            if not isinstance(rec, dict) or not isinstance(rec.get("type"), str):
                self.counts["bad_event"] += 1
                continue
            added += self._add(rec)
        if added:
            self._evict()
            self._publish()
        return added

    def _add(self, rec: dict) -> int:
        hid = rec.get("hand_id")
        key = "" if hid is None else str(hid)
        if key in self.dropped:
            self.counts["late_event"] += 1
            return 0
        h = self.hands.get(key)
        ts = _ts(rec.get("ts"))
        if h is None and len(self.hands) >= self.max_hands and ts is not None and all(
                x["first_ts"] is not None and ts < x["first_ts"] for x in self.hands.values()):
            # Older than every hand in a full window (e.g. a re-read file's early part): it
            # would be evicted at once, so it is not a change.
            self.dropped[key] = True
            self.counts["late_event"] += 1
            return 0
        if h is None:
            self.seq += 1
            h = self.hands[key] = {"hand_id": hid, "events": [], "seen": set(), "seq": self.seq,
                                   "first_ts": _ts(rec.get("ts"))}
        sig = json.dumps(rec, sort_keys=True, ensure_ascii=False)
        if sig in h["seen"]:
            self.counts["duplicate"] += 1
            return 0
        h["seen"].add(sig)
        h["events"].append(rec)
        if h["first_ts"] is None:
            h["first_ts"] = _ts(rec.get("ts"))
        self.counts["events"] += 1
        return 1

    def _order(self, key: str):
        h = self.hands[key]
        return (h["first_ts"] if h["first_ts"] is not None else 0.0, h["seq"])

    def _evict(self) -> None:
        if len(self.hands) <= self.max_hands:
            return
        for key in sorted(self.hands, key=self._order)[: len(self.hands) - self.max_hands]:
            del self.hands[key]
            self.dropped[key] = True
        while len(self.dropped) > 10000:
            self.dropped.popitem(last=False)

    def _publish(self) -> None:
        views = []
        for key in sorted(self.hands, key=self._order, reverse=True):
            h = self.hands[key]
            try:
                views.append(hand_view(h["hand_id"], h["events"]))
            except Exception as e:  # a malformed event must not take the panel down
                self.errors.appendleft({"ts": time.time(), "msg": f"{key}: {type(e).__name__}: {e}"})
                views.append({"hand_id": h["hand_id"], "error": f"{type(e).__name__}: {e}",
                              "events": h["events"]})
        with self.lock:
            self.version += 1
            self.body = {
                "enabled": True, "version": self.version, "max_hands": self.max_hands, "hands": views,
                "patterns": self.patterns,
                "files": [{"path": p, "lines": f["lines"], "bad_lines": f["bad_lines"], "mtime": f["mtime"]}
                          for p, f in sorted(self.follower.files.items())],
                "counts": dict(self.counts), "errors": list(self.errors),
            }

    def response(self, since: Optional[str]) -> dict:
        with self.lock:
            if since is not None and since == str(self.version):
                return {"enabled": True, "version": self.version, "unchanged": True, "now": time.time()}
            return dict(self.body, now=time.time())

    def run(self, stop: threading.Event, poll_s: float) -> None:
        while not stop.is_set():
            try:
                self.step()
            except Exception as e:  # keep serving the last good state
                self.errors.appendleft({"ts": time.time(), "msg": f"{type(e).__name__}: {e}"})
                traceback.print_exc()
            stop.wait(poll_s)


def _ts(v):
    return float(v) if isinstance(v, (int, float)) and not isinstance(v, bool) else None


def hand_view(hand_id, events: list[dict]) -> dict:
    """One hand's events -> the state the page shows. Only structured fields are used (the
    event's `text` is the play.log line, kept for reference). A hand whose first events were
    not read (follow started mid-hand) has partial=True."""
    v = {"hand_id": hand_id, "table_id": None, "first_ts": None, "last_ts": None, "partial": True,
         "button": None, "sb": None, "bb": None, "ante": 0, "hero_seat": None, "hole": None,
         "seats": [], "board": [], "street": "preflop", "pot": None, "turns": [], "dead": None,
         "result": None, "hero_net": None, "status": "playing", "events": events}
    seats: dict[int, dict] = {}
    turns: dict[int, dict] = {}

    def seat(s, name=None):
        x = seats.get(s)
        if x is None:
            x = seats[s] = {"seat": s, "name": name, "start": None, "stack": None, "bet": 0, "roles": [],
                            "hero": False, "folded": False, "last": None}
        if name is not None:
            x["name"] = name
        return x

    def turn(e):
        k = e.get("turn")
        t = turns.get(k)
        if t is None:
            t = turns[k] = {"turn": k, "street": e.get("street"), "pot": None, "to_call": None, "valid": None,
                            "ts": None, "decision": None, "hero_action": None}
        return t

    for e in events:
        t = e.get("type")
        ts = _ts(e.get("ts"))
        if ts is not None:
            v["first_ts"] = ts if v["first_ts"] is None else min(v["first_ts"], ts)
            v["last_ts"] = ts if v["last_ts"] is None else max(v["last_ts"], ts)
        if e.get("table_id") is not None:
            v["table_id"] = e["table_id"]
        if t == "hand_start":
            v["partial"] = False
            for k in ("button", "sb", "bb", "ante", "hero_seat"):
                v[k] = e.get(k)
        elif t == "hole":
            v["hole"] = e.get("cards")
        elif t == "seats":
            for k in ("button",):
                if e.get(k) is not None:
                    v[k] = e[k]
            for s in e.get("seats") or []:
                x = seat(s.get("seat"), s.get("name"))
                x.update(start=s.get("start"), stack=s.get("stack"), bet=s.get("bet") or 0,
                         roles=list(s.get("roles") or []), hero=bool(s.get("hero")))
                if x["hero"] and v["hero_seat"] is None:
                    v["hero_seat"] = x["seat"]
            v["pot"] = e.get("pot")
        elif t in ("action", "hero_action"):
            x = seat(e.get("seat"), e.get("name"))
            x["stack"] = e.get("stack_after")
            x["bet"] = (x["bet"] or 0) + (e.get("delta") or 0)
            if e.get("action") == "fold":
                x["folded"] = True
            x["last"] = {k: e.get(k) for k in ("action", "to", "delta", "to_call_before", "street")}
            if t == "hero_action":
                x["hero"] = True
                tr = turn(e)
                tr["hero_action"] = {k: e.get(k) for k in ("action", "to", "delta", "to_call_before",
                                                             "advice", "match", "ts")}
            v["pot"] = e.get("pot_after")
            v["street"] = e.get("street") or v["street"]
        elif t == "board":
            v["board"] = list(e.get("board") or [])
            v["street"] = e.get("street") or v["street"]
            v["pot"] = e.get("pot")
            for x in seats.values():
                x["bet"] = 0
        elif t == "hero_turn":
            tr = turn(e)
            tr.update({k: e.get(k) for k in ("street", "pot", "to_call", "valid")}, ts=ts)
            if v["hole"] is None and e.get("hole"):
                v["hole"] = e["hole"]
        elif t == "decision":
            tr = turn(e)
            if tr["street"] is None:
                tr["street"] = e.get("street")
            tr["decision"] = {k: e.get(k) for k in ("action", "amount", "all_in", "advice", "probs", "chosen",
                                                     "source", "decide_ms", "stale", "stale_kind", "written",
                                                     "rejected", "pot", "to_call", "valid")}
            tr["decision"]["ts"] = ts
        elif t == "dead":
            v["dead"] = {"reason": e.get("reason"), "tag": e.get("tag"), "ts": ts}
        elif t == "result":
            v["result"] = {k: e.get(k) for k in ("stacks", "pot", "board", "winners", "shown", "notes")}
            v["pot"] = e.get("pot")
            for x in e.get("stacks") or []:
                if x.get("seat") == v["hero_seat"]:
                    v["hero_net"] = x.get("net")
    v["seats"] = [seats[s] for s in sorted(seats, key=lambda s: (s is None, s if isinstance(s, int) else 0))]
    v["turns"] = [turns[k] for k in sorted(turns, key=lambda k: k if isinstance(k, int) else -1)]
    v["status"] = "dead" if v["dead"] else "done" if v["result"] else "playing"
    return v


# ------------------------------------------------------------------ JSON for the page

def _num(x):
    if x is None or (isinstance(x, float) and (math.isnan(x) or math.isinf(x))):
        return None
    return round(x, 4) if isinstance(x, float) else x


def _post(p: Posterior) -> dict:
    return {"k": p.k, "n": p.n, "mean": _num(p.mean), "lo": _num(p.lo), "hi": _num(p.hi)}


def _clean(v):
    if isinstance(v, Posterior):
        return _post(v)
    if isinstance(v, dict):
        return {str(k): _clean(x) for k, x in v.items()}
    if isinstance(v, (list, tuple)):
        return [_clean(x) for x in v]
    return _num(v) if isinstance(v, float) else v


def snapshot_json(res: dict, live: Live) -> dict:
    cfg: Config = res["cfg"]
    priors = res["priors"]
    seated = {s["name"] for t in live.tables.values() for s in t["seats"]
              if t["ts"] and res.get("last_ts") and res["last_ts"] - t["ts"] < 600}
    players = []
    for p in sorted(res["players"].values(), key=lambda p: -p.hands):
        hero = p.name in res["hero_names"]
        if hero:
            rows = {(s, o): posterior(*_kn(p, s, o), priors[(s, o)]) for s, o, _ in KEY_RATES}
        else:
            rows = res["rows"][p.name]
        v, cut = res["recent"][p.name]
        rr = res["recent_rows"][p.name]
        players.append({
            "name": p.name, "hero": hero, "hands": p.hands, "net_bb": round(p.net_bb, 1),
            "bb100": round(100 * p.net_bb / p.hands, 1) if p.hands else None,
            "last_ts": p.last_ts, "seated": p.name in seated, "tag": _tag(rows),
            "rates": {f"{s}:{o}": _post(rows[(s, o)]) for s, o, _ in KEY_RATES},
            "leaks": [{"name": LEAK_NAMES[x["kind"]], **_clean(x)} for x in res["leaks"].get(p.name, [])],
            "deviations": [{"label": t[3], "k": t[4], "n": t[5], "pool": _num(t[6]), "p": t[7]}
                           for t in sorted(res["deviations"].get(p.name, []), key=lambda t: t[-1])],
            "by_table": {tb: list(kn) for tb, kn in res["by_table"].get(p.name, {}).items()},
            "recent": {
                "hands": v.hands, "has_earlier": cut is not None, "since_ts": v.hand_ts[0] if v.hand_ts else None,
                "net_bb": round(v.net_bb, 1), "bb100": round(100 * v.net_bb / v.hands, 1) if v.hands else None,
                "tag": _tag(rr), "rates": {f"{s}:{o}": _post(rr[(s, o)]) for s, o, _ in KEY_RATES},
                "leaks": [{"name": LEAK_NAMES[x["kind"]], **_clean(x)} for x in res["recent_leaks"].get(p.name, [])],
            },
            "changes": [_clean(c) for c in sorted(res["changes"].get(p.name, []), key=lambda c: c["p"])],
            "n_changes": sum(c["flagged"] for c in res["changes"].get(p.name, [])),
            "mix": _clean(res["mix"].get(p.name)),
            "trend": trend(p),
            "pf_allin": _clean(allin_json(p)),
        })
    rv = res["reveal"]
    return {
        "platform": res["platform"], "hands": res["hands"], "last_ts": res.get("last_ts"),
        "built_at": time.time(), "started_at": live.started,
        "rejected": dict(res["rejected"]), "hero_names": res["hero_names"],
        "reveal": {"seats": rv["showdown_seats"], "shown": rv["shown"], "rate": _num(rv["rate"])},
        "clean_showdown": res["clean_showdown"], "forced": dict(res["forced"]),
        "cfg": {"min_hands": cfg.min_hands, "min_opps": cfg.min_opps, "bluff_pct": cfg.bluff_pct, "fdr_q": cfg.fdr_q,
                "recent_hands": cfg.recent_hands, "recent_min_opps": cfg.recent_min_opps, "mix_warn": MIX_WARN},
        "n_change_tests": res["n_change_tests"],
        "stats": [{"key": f"{s}:{o}", "label": lb, "prior": _num(priors[(s, o)].mean), "k": _num(priors[(s, o)].strength)}
                  for s, o, lb in KEY_RATES],
        "players": players,
        "tables": [dict(t, seats=[dict(x, hero=x["name"] in res["hero_names"]) for x in t["seats"]])
                   for t in sorted(live.tables.values(), key=lambda t: -(t["ts"] or 0))],
        "recent": list(live.recent),
        "files": [{"path": p, "lines": f["lines"], "bad_lines": f["bad_lines"], "mtime": f["mtime"]}
                  for p, f in sorted(live.follower.files.items())],
        "counts": dict(live.counts),
        "errors": list(live.errors),
    }


def _kn(p, stat: str, outcome: str) -> tuple[int, int]:
    c = p.counts(stat)
    return c.get(outcome, 0), sum(c.values())


# ------------------------------------------------------------------ HTTP

def make_handler(live: Live, play: Optional[PlayFeed] = None):
    class H(BaseHTTPRequestHandler):
        def log_message(self, fmt, *args):  # quiet: the page polls every few seconds
            pass

        def _send(self, code: int, body: bytes, ctype: str) -> None:
            self.send_response(code)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            u = urlsplit(self.path)
            with live.lock:
                snap, md, prof = live.snapshot, live.md, live.profile
            if u.path == "/":
                with open(PAGE_PATH, "rb") as f:
                    self._send(200, f.read(), "text/html; charset=utf-8")
            elif u.path == "/api/snapshot":
                since = parse_qs(u.query).get("since", [None])[0]
                if not snap:
                    body = {"loading": True}
                elif since is not None and since == str(snap.get("version")):
                    body = {"version": snap.get("version"), "unchanged": True}
                else:
                    body = snap
                self._send(200, json.dumps(body, ensure_ascii=False).encode(), "application/json; charset=utf-8")
            elif u.path == "/api/play":
                since = parse_qs(u.query).get("since", [None])[0]
                body = play.response(since) if play is not None else {"enabled": False}
                self._send(200, json.dumps(body, ensure_ascii=False).encode(), "application/json; charset=utf-8")
            elif u.path == "/report.md":
                self._send(200, md.encode(), "text/markdown; charset=utf-8")
            elif u.path == "/profile.json":
                self._send(200, json.dumps(prof, ensure_ascii=False, indent=1).encode(), "application/json; charset=utf-8")
            else:
                self._send(404, b"not found", "text/plain; charset=utf-8")

    return H


def serve(db_dir: str, platform: str, patterns: list[str], host: str, port: int, cfg: Config,
          poll_s: float = 2.0, refresh_s: float = 5.0, hero_names=(), play_patterns=None,
          play_hands: int = 20, play_poll_s: float = 0.5) -> None:
    live = Live(db_dir, platform, patterns, cfg, refresh_s, hero_names)
    play = PlayFeed(play_patterns, play_hands) if play_patterns else None
    # Bind first: a busy port fails at once, and the page shows "loading" during bootstrap.
    httpd = ThreadingHTTPServer((host, port), make_handler(live, play))
    print(f"serving http://{host}:{port}/", flush=True)
    stop = threading.Event()
    failed = []
    if play is not None:
        # Own thread: the guide stays live while the profile bootstraps or rebuilds.
        threading.Thread(target=play.run, args=(stop, play_poll_s), daemon=True).start()
        print(f"action guide: following {', '.join(play_patterns)} (last {play.max_hands} hands)", flush=True)

    def work() -> None:
        t0 = time.time()
        try:
            live.bootstrap()
        except Exception:  # bad database / unreadable log: stop loudly, never serve a partial profile
            traceback.print_exc()
            failed.append(1)
            httpd.shutdown()
            return
        print(f"{platform}: {live.acc.n} hands loaded in {time.time() - t0:.1f}s; following "
              f"{len(live.follower.files)} file(s): {', '.join(patterns)}", flush=True)
        live.run(stop, poll_s)

    threading.Thread(target=work, daemon=True).start()
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        stop.set()
        httpd.server_close()
        live.close()
    if failed:
        raise SystemExit("bootstrap failed")

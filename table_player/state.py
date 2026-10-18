"""Read-only reduction of OpenPoker WS logs into a public table plus hero cards."""

from copy import deepcopy
import json
import math
import re
from pathlib import Path


CARD = re.compile(r"^[2-9TJQKA][cdhs]$")
EVENTS = {
    "connected", "lobby_joined", "table_joined", "table_closed", "player_joined",
    "player_left", "hand_start", "hole_cards", "table_state", "community_cards",
    "player_action", "your_turn", "hand_result", "resync_response", "error",
}


def cards(value):
    return [c for c in value if isinstance(c, str) and CARD.fullmatch(c)] if isinstance(value, list) else []


def seat_id(value):
    return value if isinstance(value, int) and not isinstance(value, bool) and 0 <= value < 9 else None


def number(value):
    return value if isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value) else None


def player(seat):
    return dict(seat=seat, name=None, stack=None, bet=0, status="empty",
                in_hand=False, folded=False, cards=[], last_action=None, winner=False)


def normalize(record):
    if not isinstance(record, dict):
        return None
    if record.get("dir") == "event":
        kind = record.get("event")
        if kind not in ("open", "close", "error"):
            return None
        # Never expose raw exceptions, headers, auth tokens, or HTML to the browser.
        error = str(record.get("error", ""))
        message = "连接错误"
        match = re.search(r"Handshake status (\d{3})", error)
        if match:
            message = f"WebSocket HTTP {match.group(1)}"
        return {"type": "connection", "kind": kind, "message": message, "ts": record.get("ts")}
    if "dir" in record:
        if record["dir"] != "in":
            return None
        raw = record.get("raw")
        message = json.loads(raw) if isinstance(raw, str) else raw
    else:
        message = record
    if not isinstance(message, dict) or message.get("type") not in EVENTS:
        return None
    # Only these protocol fields are needed for rendering; credentials never enter the cache.
    keys = ("type", "table_id", "hand_id", "table_seq", "stream", "seat", "name", "players",
            "seats", "hero", "dealer_seat", "blinds", "small_blind", "big_blind", "pot",
            "pot_after", "total_pot", "actor_seat", "board", "street", "cards",
            "community_cards", "action", "amount", "amount_mode", "stack", "stack_before",
            "stack_after", "contribution_delta", "to_call_before", "winners", "shown_cards",
            "final_stacks", "code", "snapshot", "replayed_events", "ts", "role")
    result = {k: message[k] for k in keys if k in message}
    # Nested snapshots are reduced through the same whitelist.
    if isinstance(result.get("hero"), dict):
        result["hero"] = {k: result["hero"][k] for k in ("seat", "hole_cards") if k in result["hero"]}
    if isinstance(result.get("snapshot"), dict):
        result["snapshot"] = normalize({**result["snapshot"], "type": "table_state"})
    if isinstance(result.get("replayed_events"), list):
        result["replayed_events"] = [m for e in result["replayed_events"] if (m := normalize(e))]
    result["ts"] = record.get("ts", result.get("ts"))
    return result


class TableState:
    def __init__(self, seats=9, hero_seat=None):
        self.default_seats = seats
        self.default_hero = hero_seat
        self.event_seq = -1
        self.snapshot_seq = -1
        self.last_active = None
        self.data = dict(table_id=None, hand_id=None, hand_number=0, hero_seat=hero_seat,
                         hero_name=None, hole=[], num_seats=seats, players=[player(i) for i in range(seats)],
                         board=[], street="idle", pot=0, dealer_seat=None, actor_seat=None,
                         small_blind=10, big_blind=20, connection="waiting", notice="等待牌局日志",
                         history=[], result=None, ts=None)

    def log(self, kind, text, seat=None, amount=None):
        self.data["history"].append(dict(kind=kind, text=text, seat=seat, amount=amount,
                                         street=self.data["street"]))
        self.data["history"] = self.data["history"][-40:]

    def get_player(self, seat):
        seat = seat_id(seat)
        if seat is None:
            return None
        while seat >= len(self.data["players"]):
            self.data["players"].append(player(len(self.data["players"])))
        self.data["num_seats"] = len(self.data["players"])
        return self.data["players"][seat]

    def new_hand(self, hand_id):
        d = self.data
        if not hand_id or hand_id == d["hand_id"]:
            return
        self.last_active = None
        d.update(hand_id=hand_id, hand_number=d["hand_number"] + 1, hole=[], board=[],
                 pot=0, street="preflop", actor_seat=None, result=None, history=[])
        for p in d["players"]:
            p.update(bet=0, in_hand=False, folded=False, cards=[], last_action=None, winner=False)
        self.log("hand", "新的一手")

    def set_hero(self, seat):
        if seat_id(seat) is not None:
            if self.data["hero_seat"] != seat:
                self.data["hole"] = []
            self.data["hero_seat"] = seat

    def street(self, value):
        if isinstance(value, int) and 0 <= value <= 3:
            value = ("preflop", "flop", "turn", "river")[value]
        if value not in ("idle", "preflop", "flop", "turn", "river", "showdown"):
            return
        if value != self.data["street"]:
            for p in self.data["players"]:
                p.update(bet=0, last_action=None)
        self.data["street"] = value

    def merge_player(self, incoming, snapshot=False):
        if not isinstance(incoming, dict):
            return
        p = self.get_player(incoming.get("seat"))
        if p is None:
            return
        if incoming.get("status") == "empty":
            p.update(player(p["seat"]))
            return
        if incoming.get("name") and p["name"] != incoming["name"]:
            p.update(player(p["seat"]))
        for key in ("name", "status", "in_hand", "folded"):
            if key in incoming:
                p[key] = incoming[key]
        for key in ("stack", "bet"):
            if number(incoming.get(key)) is not None:
                p[key] = incoming[key]
        if p["name"] and p["status"] == "empty":
            p["status"] = "active"
        if snapshot:
            p["folded"] = bool(incoming.get("folded", False))
            p["in_hand"] = bool(incoming.get("in_hand", False))

    def apply(self, m):
        d = self.data
        t = m.get("type")
        if t == "connection":
            d["connection"] = {"open": "connecting", "close": "disconnected", "error": "error"}[m["kind"]]
            if m["kind"] == "error":
                d["notice"] = m["message"]
            elif m["kind"] == "close":
                d["notice"] = "连接已关闭，保留最后牌桌"
            d["ts"] = m.get("ts")
            return
        if t == "resync_response":
            for event in sorted(m.get("replayed_events") or [], key=lambda e: e.get("table_seq") or 0):
                self.apply(event)
            if m.get("snapshot"):
                self.apply(m["snapshot"])
            return
        tid = m.get("table_id")
        if tid and tid != d["table_id"]:
            if d["table_id"] and t not in ("table_joined", "table_state", "hand_start"):
                return
            name, count = d["hero_name"], d["hand_number"]
            self.__init__(self.default_seats, self.default_hero)
            d = self.data
            d.update(table_id=tid, hero_name=name, hand_number=count)
        seq = m.get("table_seq")
        if isinstance(seq, int):
            if t == "table_state":
                if seq <= self.snapshot_seq or seq < self.event_seq:
                    return
                self.snapshot_seq = seq
            else:
                if seq <= self.event_seq:
                    return
                self.event_seq = seq
        stale = isinstance(seq, int) and seq <= self.snapshot_seq and t != "table_state"
        d["ts"] = m.get("ts") or d["ts"]
        d["connection"] = "connected"
        d["notice"] = ""
        if t == "connected":
            d["hero_name"] = m.get("name")
        elif t == "lobby_joined":
            d["notice"] = "大厅排队中"
            d["actor_seat"] = None
        elif t == "table_closed":
            d.update(notice="牌桌已关闭，等待重新入座", actor_seat=None, hole=[], board=[], hand_id=None)
            d["players"] = [player(i) for i in range(d["num_seats"])]
        elif t in ("table_joined", "hand_start"):
            self.set_hero(m.get("seat"))
            if not stale:
                self.new_hand(m.get("hand_id"))
                if "dealer_seat" in m:
                    d["dealer_seat"] = seat_id(m["dealer_seat"])
                for p in m.get("players") or []:
                    self.merge_player(p)
                for key in ("small_blind", "big_blind"):
                    if number((m.get("blinds") or {}).get(key)) is not None:
                        d[key] = m["blinds"][key]
        elif t == "table_state":
            self.new_hand(m.get("hand_id"))
            # OpenPoker can send its idle/empty-board snapshot BEFORE hand_result.
            # Keep the last active board to show that result without future leakage.
            if m.get("street") == "idle" and d["street"] not in ("idle", "showdown"):
                self.last_active = deepcopy(d)
            self.street(m.get("street"))
            d["board"] = cards(m.get("board"))
            if number(m.get("pot")) is not None:
                d["pot"] = m["pot"]
            d["actor_seat"] = seat_id(m.get("actor_seat"))
            d["dealer_seat"] = seat_id(m.get("dealer_seat"))
            for key in ("small_blind", "big_blind"):
                if number(m.get(key)) is not None:
                    d[key] = m[key]
            seats = m.get("seats")
            if isinstance(seats, list) and seats:
                n = max((seat_id(p.get("seat")) or 0) + 1 for p in seats if isinstance(p, dict))
                if 2 <= n <= 9:
                    d["players"] = (d["players"] + [player(i) for i in range(len(d["players"]), n)])[:n]
                    d["num_seats"] = n
                for p in seats:
                    self.merge_player(p, snapshot=True)
            hero = m.get("hero") or {}
            self.set_hero(hero.get("seat"))
            if "hole_cards" in hero:
                d["hole"] = cards(hero["hole_cards"])[:2]
        elif t == "hole_cards":
            if not m.get("hand_id") or m["hand_id"] == d["hand_id"]:
                d["hole"] = cards(m.get("cards"))[:2]
                p = self.get_player(d["hero_seat"])
                if p:
                    p["in_hand"] = True
        elif t == "player_joined":
            self.merge_player(m)
        elif t == "player_left":
            p = self.get_player(m.get("seat"))
            if p:
                p.update(player(p["seat"]))
                if p["seat"] == d["hero_seat"]:
                    d.update(hole=[], hero_seat=None, actor_seat=None, notice="Hero 已离桌")
        elif t == "community_cards" and not stale:
            incoming = cards(m.get("cards"))
            self.street(m.get("street"))
            if len(incoming) >= 3:
                d["board"] = incoming[:5]
            else:
                expected = {"flop": 3, "turn": 4, "river": 5}.get(d["street"], 0)
                if len(d["board"]) < expected:
                    d["board"] = (d["board"] + incoming)[:expected]
            self.log("board", " ".join(d["board"]))
        elif t == "your_turn" and not stale:
            self.new_hand(m.get("hand_id"))
            self.set_hero(m.get("seat"))
            d["actor_seat"] = d["hero_seat"]
            if "community_cards" in m:
                d["board"] = cards(m["community_cards"])
                self.street({0: "preflop", 3: "flop", 4: "turn", 5: "river"}.get(len(d["board"])))
            if number(m.get("pot")) is not None:
                d["pot"] = m["pot"]
            for p in m.get("players") or []:
                self.merge_player(p)
        elif t == "player_action":
            p = self.get_player(m.get("seat"))
            if p is None or (m.get("hand_id") and m["hand_id"] != d["hand_id"]):
                return
            action = m.get("action", "")
            amount = m.get("contribution_delta") if action in ("call", "all_in") else m.get("amount")
            self.log("action", action, p["seat"], number(amount))
            if stale:
                return
            # Closing actions may be labelled with the NEXT street. Board events and
            # snapshots are authoritative for street transitions, not player_action.street.
            delta = number(m.get("contribution_delta"))
            before, after = number(m.get("stack_before")), number(m.get("stack_after"))
            if delta is None and before is not None and after is not None:
                delta = max(0, before - after)
            if delta is None and number(m.get("amount")) is not None:
                delta = max(0, m["amount"] - p["bet"]) if m.get("amount_mode") == "to_total" else m["amount"]
            if delta is None and action == "call":
                delta = number(m.get("to_call_before"))
            if delta is not None:
                p["bet"] += delta
            if after is not None:
                p["stack"] = after
            elif action == "all_in":
                p["stack"] = 0
            elif number(m.get("stack")) is not None:
                p["stack"] = m["stack"]
            p["last_action"] = action
            p["folded"] = action == "fold" or p["folded"]
            p["in_hand"] = True
            d["actor_seat"] = None
            pot = number(m.get("pot_after", m.get("pot")))
            if pot is not None:
                d["pot"] = pot
        elif t == "hand_result":
            if m.get("hand_id") and m["hand_id"] != d["hand_id"]:
                return
            if self.last_active and self.last_active["hand_id"] == d["hand_id"]:
                d["board"] = self.last_active["board"]
                for p in d["players"]:
                    old = next((old for old in self.last_active["players"] if old["seat"] == p["seat"] and old["name"] == p["name"]), None)
                    if old:
                        for key in ("in_hand", "folded", "last_action"):
                            p[key] = old[key]
            d["actor_seat"] = None
            d["street"] = "showdown"
            d["result"] = {"winners": [], "pot": m.get("total_pot", m.get("pot", d["pot"]))}
            d["pot"] = d["result"]["pot"]
            for s, stack in (m.get("final_stacks") or {}).items():
                p = self.get_player(int(s)) if str(s).isdigit() else None
                if p and number(stack) is not None:
                    p["stack"] = stack
            for p in d["players"]:
                p["bet"] = 0
            for winner in m.get("winners") or []:
                p = self.get_player(winner.get("seat"))
                if p:
                    p["winner"] = True
                    d["result"]["winners"].append({"seat": p["seat"], "name": winner.get("name") or p["name"],
                                                    "amount": winner.get("amount"), "description": winner.get("hand_description")})
            shown = m.get("shown_cards") or {}
            if isinstance(shown, dict):
                for s, hole in shown.items():
                    p = self.get_player(int(s)) if str(s).isdigit() else None
                    if p:
                        p["cards"] = cards(hole)[:2]
            self.log("result", "本手结束")
        elif t == "error":
            d["notice"] = "服务端：" + str(m.get("code", "error"))[:80]


class LogStore:
    """Incrementally tail JSONL; replay from sparse checkpoints, never from future state."""

    def __init__(self, path, seats=9, hero_seat=None):
        self.path = Path(path)
        self.seats, self.hero_seat = seats, hero_seat
        self.generation = 0
        self.reset()

    def reset(self):
        self.events = []
        self.live = TableState(self.seats, self.hero_seat)
        self.checkpoints = {0: deepcopy(self.live)}
        self.hand_starts = []
        self.offset, self.pending, self.identity = 0, b"", None
        self.invalid, self.mtime, self.missing = 0, None, False
        self.generation += 1

    def refresh(self):
        try:
            with self.path.open("rb") as stream:
                import os
                stat = os.fstat(stream.fileno())
                identity = (stat.st_dev, stat.st_ino)
                if self.identity != identity or stat.st_size < self.offset:
                    self.reset()
                self.identity, self.mtime, self.missing = identity, stat.st_mtime, False
                stream.seek(self.offset)
                while chunk := stream.read(1024 * 1024):
                    self.offset += len(chunk)
                    lines = (self.pending + chunk).split(b"\n")
                    self.pending = lines.pop()
                    for line in lines:
                        if not line.strip():
                            continue
                        try:
                            event = normalize(json.loads(line))
                            if event is None:
                                continue
                            previous = self.live.data["hand_number"]
                            self.live.apply(event)
                        except (ValueError, TypeError, KeyError, AttributeError):
                            self.invalid += 1
                            continue
                        self.events.append(event)
                        count = len(self.events)
                        if self.live.data["hand_number"] != previous:
                            self.hand_starts.append({"cursor": count, "id": self.live.data["hand_id"],
                                                     "number": self.live.data["hand_number"]})
                        if count % 128 == 0:
                            self.checkpoints[count] = deepcopy(self.live)
        except FileNotFoundError:
            self.missing = True

    def view(self, cursor=None):
        count = len(self.events)
        cursor = count if cursor is None else max(0, min(cursor, count))
        if cursor == count:
            state = self.live.data
        else:
            start = cursor // 128 * 128
            model = deepcopy(self.checkpoints[start])
            for event in self.events[start:cursor]:
                model.apply(event)
            state = model.data
        return {"state": deepcopy(state), "meta": {"total": count, "cursor": cursor,
                "generation": self.generation, "hand_starts": self.hand_starts,
                "invalid_lines": self.invalid, "partial_line": bool(self.pending),
                "mtime": self.mtime, "missing": self.missing}}

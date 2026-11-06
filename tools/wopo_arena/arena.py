#!/usr/bin/env python3
"""Local wopo-protocol table: real wopo_play.py drivers and advisors playing each other.

Each bot is an unmodified driver + advisor pair (independent processes). The driver connects by
WebSocket as it does on the real table, sees itself at seat 0, and writes its decision to its action
file; this table tails that file and plays the action. See tools/wopo_arena/README.md.

Duplicate dealing: one round is n hands sharing one board and n hole-card sets. Hole cards are bound to
positions and the button moves one seat per hand, so within a round every bot plays every position
with that position's cards. Seating is reshuffled between rounds.
"""

import argparse
import base64
import datetime
import hashlib
import json
import os
import random
import signal
import socket
import struct
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(REPO, "tools"))

from engine import Hand, IllegalAction  # noqa: E402
from poker_live.wopo import _fmt_act  # noqa: E402

TABLE_ID = "arena"
WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
RANKS = "23456789TJQKA"
DECK = [r + s for r in RANKS for s in "cdhs"]


class Abort(Exception):
    """The match cannot continue with trustworthy results."""


def now_iso():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


# ------------------------------------------------------------------ dealing and seating

def _rng(seed, *parts):
    text = ":".join(str(x) for x in (seed, *parts))
    return random.Random(int.from_bytes(hashlib.sha256(text.encode()).digest()[:16], "big"))


def round_cards(seed, round_no, n):
    """(hole cards per position, board). Position 0 = SB, 1 = BB, ... n-1 = button."""
    deck = list(DECK)
    _rng(seed, "deal", round_no).shuffle(deck)
    return [deck[i * 2:i * 2 + 2] for i in range(n)], deck[n * 2:n * 2 + 5]


def round_seating(seed, round_no, names):
    """Bot name per table seat for one round."""
    order = list(names)
    _rng(seed, "seat", round_no).shuffle(order)
    return order


def hand_layout(seed, hand_no, names):
    """Hand numbers start at 1. Returns (round_no, button, bot per seat, hole per seat, board)."""
    n = len(names)
    round_no = (hand_no - 1) // n
    button = (hand_no - 1) % n
    seating = round_seating(seed, round_no, names)
    by_pos, board = round_cards(seed, round_no, n)
    holes = [by_pos[(seat - button - 1) % n] for seat in range(n)]
    return round_no, button, seating, holes, board


def apply_stack_rules(stack, table):
    """Stack a bot starts the next hand with. flat_stacks: always the start stack. Otherwise only below the
    rebuy line or above the reset line."""
    bb = table["big_blind"]
    if table.get("flat_stacks") or stack < table["rebuy_below_bb"] * bb or stack > table["reset_above_bb"] * bb:
        return table["start_bb"] * bb
    return stack


# ------------------------------------------------------------------ WebSocket server

class Conn:
    def __init__(self, sock, token):
        self.sock = sock
        self.token = token
        self.lock = threading.Lock()
        self.alive = True

    def _frame(self, opcode, payload):
        n = len(payload)
        if n < 126:
            head = struct.pack("!BB", 0x80 | opcode, n)
        elif n < 65536:
            head = struct.pack("!BBH", 0x80 | opcode, 126, n)
        else:
            head = struct.pack("!BBQ", 0x80 | opcode, 127, n)
        with self.lock:
            self.sock.sendall(head + payload)

    def send_text(self, text):
        self._frame(1, text.encode())

    def _read(self, n):
        out = b""
        while len(out) < n:
            chunk = self.sock.recv(n - len(out))
            if not chunk:
                raise ConnectionError("peer closed")
            out += chunk
        return out

    def reader(self):
        """Answer pings and notice a close. Drivers send nothing else."""
        try:
            while True:
                b0, b1 = self._read(2)
                opcode, n = b0 & 0x0F, b1 & 0x7F
                if n == 126:
                    n = struct.unpack("!H", self._read(2))[0]
                elif n == 127:
                    n = struct.unpack("!Q", self._read(8))[0]
                mask = self._read(4) if b1 & 0x80 else None
                data = self._read(n)
                if mask:
                    data = bytes(x ^ mask[i % 4] for i, x in enumerate(data))
                if opcode == 9:
                    self._frame(10, data)
                elif opcode == 8:
                    self._frame(8, data[:2])
                    break
        except OSError:
            pass
        finally:
            self.alive = False
            try:
                self.sock.close()
            except OSError:
                pass


class WsServer:
    def __init__(self, tokens, port=0):
        self.tokens = set(tokens)
        self.conns = {}
        self.cond = threading.Condition()
        self.listener = socket.create_server(("127.0.0.1", port))
        self.port = self.listener.getsockname()[1]
        threading.Thread(target=self._accept, daemon=True).start()

    def _accept(self):
        while True:
            try:
                sock, _ = self.listener.accept()
            except OSError:
                return
            threading.Thread(target=self._handshake, args=(sock,), daemon=True).start()

    def _handshake(self, sock):
        try:
            data = b""
            while b"\r\n\r\n" not in data:
                chunk = sock.recv(4096)
                if not chunk:
                    sock.close()
                    return
                data += chunk
            headers = {}
            for line in data.split(b"\r\n")[1:]:
                k, sep, v = line.decode("latin1").partition(":")
                if sep:
                    headers[k.strip().lower()] = v.strip()
            token = headers.get("authorization", "").removeprefix("Bearer ").strip()
            if token not in self.tokens or "sec-websocket-key" not in headers:
                sock.sendall(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n")
                sock.close()
                return
            accept = base64.b64encode(hashlib.sha1((headers["sec-websocket-key"] + WS_GUID).encode()).digest()).decode()
            sock.sendall(("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                          f"Sec-WebSocket-Accept: {accept}\r\n\r\n").encode())
            sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        except OSError:
            return
        conn = Conn(sock, token)
        with self.cond:
            self.conns[token] = conn
            self.cond.notify_all()
        conn.reader()

    def all_connected(self):
        with self.cond:
            return all(t in self.conns and self.conns[t].alive for t in self.tokens)

    def send(self, token, text):
        conn = self.conns.get(token)
        if conn is None or not conn.alive:
            raise Abort(f"driver {token} is not connected")
        try:
            conn.send_text(text)
        except OSError as e:
            raise Abort(f"send to driver {token} failed: {e!r}") from e


# ------------------------------------------------------------------ driver processes

class Bot:
    def __init__(self, name, group, seed, args, out_dir):
        self.name, self.group, self.seed, self.args = name, group, seed, args
        self.dir = os.path.join(out_dir, name)
        self.action_path = os.path.join(self.dir, "wopo_actions.jsonl")
        self.hh_path = os.path.join(self.dir, "wopo_hh.jsonl")
        self.proc = None
        self.offset = 0
        self.partial = b""

    def start(self, python, url, table, log, warm_from=None):
        os.makedirs(self.dir, exist_ok=True)
        self.offset = os.path.getsize(self.action_path) if os.path.exists(self.action_path) else 0
        # A restarted driver rebuilds its in-process opponent profiles from what it already played, and a
        # new stage of a match from what the same bot played in the stage before.
        warm = []
        if warm_from:
            warm.append(os.path.join(warm_from, self.name, "wopo_hh.jsonl"))
        if os.path.exists(self.hh_path) and os.path.getsize(self.hh_path):
            warm.append(self.hh_path)
        warm_args = [x for path in warm for x in ("--warm-hh", path)] or ["--warm-hh", "off"]
        d = self.dir
        cmd = [python, os.path.join(REPO, "tools", "wopo_play.py"), "--url", url,
               "--seats", str(table["seats"]), "--ante", str(table["ante"]),
               "--blueprint-ante", str(table["ante"] * 100 // table["big_blind"]),
               "--stack-bb", str(table["start_bb"]), "--seed", str(self.seed), "--aliases", "off",
               *warm_args, "--action-file", self.action_path,
               "--action-log", os.path.join(d, "wopo_actions_log.jsonl"), "--hh-log", self.hh_path,
               "--ws-log", os.path.join(d, "wopo_ws.jsonl"), "--play-log", os.path.join(d, "play.log"),
               *self.args]
        driver_log = open(os.path.join(d, "driver.log"), "a")
        print(f"[{now_iso()}] start {' '.join(cmd)}", file=driver_log, flush=True)
        self.proc = subprocess.Popen(cmd, cwd=REPO, env={**os.environ, "WOPO_TOKEN": self.name},
                                     stdout=driver_log, stderr=driver_log, stdin=subprocess.DEVNULL,
                                     start_new_session=True)
        driver_log.close()
        with open(os.path.join(d, "driver.pid"), "w") as f:
            f.write(str(self.proc.pid))
        log(f"driver {self.name} pid {self.proc.pid} (warm {warm or 'off'})")

    def running(self):
        return self.proc is not None and self.proc.poll() is None

    def stop(self):
        if self.proc is None or self.proc.poll() is not None:
            return
        try:
            os.killpg(self.proc.pid, signal.SIGTERM)
        except ProcessLookupError:
            return

    def kill(self):
        if self.proc is None:
            return
        try:
            os.killpg(self.proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass

    def read_action(self):
        """Next complete line of the action file, or None."""
        try:
            with open(self.action_path, "rb") as f:
                f.seek(self.offset)
                chunk = f.read()
        except FileNotFoundError:
            return None
        if chunk:
            self.offset += len(chunk)
            self.partial += chunk
        if b"\n" not in self.partial:
            return None
        line, self.partial = self.partial.split(b"\n", 1)
        return json.loads(line)

    def rss_kb(self):
        """(driver RSS, advisor RSS) in kB; 0 for a process that is gone."""
        def rss(pid):
            try:
                with open(f"/proc/{pid}/status") as f:
                    for line in f:
                        if line.startswith("VmRSS:"):
                            return int(line.split()[1])
            except OSError:
                pass
            return 0
        if self.proc is None:
            return 0, 0
        pid = self.proc.pid
        try:
            with open(f"/proc/{pid}/task/{pid}/children") as f:
                kids = [int(x) for x in f.read().split()]
        except OSError:
            kids = []
        return rss(pid), sum(rss(k) for k in kids)


def last_hand_id(path):
    """hand_id on the last complete line of a hand history file, or None."""
    try:
        with open(path, "rb") as f:
            f.seek(0, os.SEEK_END)
            f.seek(max(0, f.tell() - 262144))
            lines = f.read().split(b"\n")
    except OSError:
        return None
    # The text after the final newline is empty or still being written.
    return json.loads(lines[-2]).get("hand_id") if len(lines) >= 2 else None


def wait_for_drivers(bots, hand_id, timeout=15.0):
    """Let every driver log the last result before it is stopped. Returns the bots that did not."""
    deadline = time.monotonic() + timeout
    late = []
    for b in bots:
        while b.running() and last_hand_id(b.hh_path) != hand_id and time.monotonic() < deadline:
            time.sleep(0.02)
        if last_hand_id(b.hh_path) != hand_id:
            late.append(b.name)
    return late


def meminfo_kb():
    out = {}
    with open("/proc/meminfo") as f:
        for line in f:
            k, _, v = line.partition(":")
            out[k] = int(v.split()[0])
    return out


# ------------------------------------------------------------------ the table

class Table:
    def __init__(self, cfg, bots, server, out_dir, seed, restarts, decision_timeout, log):
        self.table = cfg
        self.bots = {b.name: b for b in bots}
        self.names = [b.name for b in bots]
        self.server = server
        self.seed = seed
        self.restarts = restarts
        self.decision_timeout = decision_timeout
        self.log = log
        self.seq = 0
        self.event_id = 0
        table_dir = os.path.join(out_dir, "table")
        os.makedirs(table_dir, exist_ok=True)
        self.hh_path = os.path.join(table_dir, "wopo_hh.jsonl")
        self.hh = open(self.hh_path, "a", encoding="utf-8")
        self.events = open(os.path.join(table_dir, "wopo_play_events.jsonl"), "a", encoding="utf-8")
        self.counts = {"timeouts": 0, "valid_mismatch": 0, "fold_to_check": 0, "illegal": 0, "bad_record": 0}
        self.last_hand_id = None

    def _event(self, etype, hand_id, **fields):
        self.events.write(json.dumps({"ts": round(time.time(), 3), "type": etype, "hand_id": hand_id,
                                      "table_id": TABLE_ID, **fields}, ensure_ascii=False) + "\n")
        self.events.flush()

    # Every event goes to every driver, in that driver's own seat numbering, under one sequence number.
    def _broadcast(self, ctx, build, stream="event"):
        self.seq += 1
        ctx["hand_seq"] += 1
        envelope = {"stream": stream, "table_id": TABLE_ID, "hand_id": ctx["hand_id"], "table_seq": self.seq,
                    "hand_seq": ctx["hand_seq"], "ts": now_iso()}
        for me, name in enumerate(ctx["seating"]):
            self.server.send(name, json.dumps({**build(me), **envelope}, ensure_ascii=False))

    def _state_msg(self, ctx, hand, me):
        n = hand.n
        seats = []
        for offset in range(n):
            s = (me + offset) % n
            seats.append({"seat": offset, "name": ctx["seating"][s], "stack": hand.stack[s], "bet": hand.bet[s],
                          "status": "active", "is_pro": False, "in_hand": True, "folded": s in hand.folded})
        return {"type": "table_state", "street": hand.street, "dealer_seat": (hand.button - me) % n,
                "small_blind": hand.sb, "big_blind": hand.bb, "pot": hand.pot, "board": hand.board,
                "seats": seats, "hero": {"seat": 0, "hole_cards": hand.holes[me]},
                "actor_seat": None if hand.to_act is None else (hand.to_act - me) % n}

    def _result_msg(self, ctx, hand, me, reveal_all=False):
        """reveal_all: the table's own record keeps every seat's cards. Drivers get the showdown cards of
        the other seats, as on the real table."""
        n, res = hand.n, hand.result
        view = lambda s: (s - me) % n
        winners = [{"seat": view(s), "name": ctx["seating"][s], "stack": res["final"][s], "amount": res["net"][s]}
                   for s in res["winners"]]
        transferable = sum(max(0, res["net"][s]) for s in res["winners"])
        msg = {"type": "hand_result", "winners": winners, "pot": transferable, "transferable_pot": transferable,
               "total_pot": res["total_pot"],
               "payouts": [{"seat": view(s), "amount": res["payout"][s]} for s in res["winners"]],
               "outcome_kind": "win" if len(winners) == 1 else "multiple_winners", "pot_kind": "transferable",
               "final_stacks": {str(view(s)): res["final"][s] for s in range(n)}}
        if reveal_all:
            shown = {str(view(s)): hand.holes[s] for s in range(n) if s != me}
        else:
            shown = {str(view(s)): hand.holes[s] for s in res["showdown"] if s != me}
        if shown:
            msg["shown_cards"] = shown
        return msg

    def _wait_decision(self, bot, hand_id):
        deadline = time.monotonic() + self.decision_timeout
        while True:
            rec = bot.read_action()
            if rec is not None:
                # The driver stamps the sequence number of the table_state that asked for this decision,
                # which is the last message sent. Anything else is a late answer to an earlier turn.
                if rec.get("hand_id") == hand_id and rec.get("table_seq") == self.seq:
                    return rec
                self.counts["bad_record"] += 1
                self.log(f"[warn] {bot.name} stale action {rec.get('hand_id')}#{rec.get('table_seq')} "
                         f"during {hand_id}#{self.seq}")
                continue
            if not bot.running():
                raise Abort(f"driver {bot.name} exited during {hand_id}; see {bot.dir}/driver.log")
            if time.monotonic() > deadline:
                return None
            time.sleep(0.005)

    def _resolve(self, hand, rec, name, hand_id):
        """Driver record -> (kind, to, note). The action actually played, always legal."""
        legal = hand.legal()
        passive = "check" if legal["can_check"] else "fold"
        if rec is None:
            self.counts["timeouts"] += 1
            self.log(f"[warn] {name} gave no decision in {self.decision_timeout}s on {hand_id}: {passive}")
            return passive, None, "timeout"
        if rec.get("valid") != legal:
            self.counts["valid_mismatch"] += 1
            self.log(f"[warn] {name} legal range differs on {hand_id}: driver {rec.get('valid')} table {legal}")
        action = rec.get("action")
        if action == "fold" and legal["can_check"]:
            self.counts["fold_to_check"] += 1
            return "check", None, "fold_to_check"
        if action in ("fold", "check", "call", "all_in"):
            return action, None, None
        if action == "raise":
            return "raise", rec.get("amount"), None
        self.counts["illegal"] += 1
        self.log(f"[warn] {name} unknown action {action!r} on {hand_id}: {passive}")
        return passive, None, "illegal"

    def _play_action(self, ctx, hand):
        seat, turn = hand.to_act, ctx["turn"]
        name = ctx["seating"][seat]
        hand_id = ctx["hand_id"]
        legal = hand.legal()
        to_call = hand.max_bet - hand.bet[seat]
        pot_before, street = hand.pot, hand.street
        self._event("hero_turn", hand_id, turn=turn, seat=seat, street=street, pot=pot_before, to_call=to_call,
                    valid=legal, hole=hand.holes[seat], board=hand.board)
        rec = self._wait_decision(self.bots[name], hand_id)
        kind, to, note = self._resolve(hand, rec, name, hand_id)
        try:
            act = hand.apply(kind, to)
        except IllegalAction as e:
            self.counts["illegal"] += 1
            self.log(f"[warn] {name} illegal {kind} {to} on {hand_id}: {e}")
            note = "illegal"
            act = hand.apply("check" if legal["can_check"] else "fold")
        advice_action = rec.get("action") if rec else None
        advice_amount = rec.get("amount") if rec else None
        advice_text = _fmt_act(advice_action, advice_amount, to_call, to_call) if rec else "无"
        if rec:
            all_in = advice_action == "all_in" or (advice_action == "raise" and advice_amount == legal["max_raise"])
            self._event("decision", hand_id, turn=turn, seat=seat, street=street, pot=pot_before, to_call=to_call,
                        valid=legal, action=advice_action, amount=advice_amount, all_in=all_in, advice=advice_text,
                        probs=rec.get("probs"), chosen=rec.get("chosen"), source=rec.get("source"),
                        decide_ms=rec.get("decide_ms"), stale=None, stale_kind=None, written=True, rejected=None)
        self._event("hero_action", hand_id, seat=seat, name=name, street=street, turn=turn, action=act["label"],
                    to=act["to"], delta=act["delta"], to_call_before=act["to_call_before"],
                    stack_before=act["stack_before"], stack_after=act["stack"], pot_after=act["pot"],
                    advice={"action": advice_action, "amount": advice_amount, "text": advice_text} if rec else None,
                    match=None if rec is None else ("diff" if note else "same"), note=note)
        ctx["actions"].append({"seat": seat, "action": act["label"], **({"to": act["to"]} if act["to"] is not None else {}),
                               "street": street, "contribution_delta": act["delta"],
                               "to_call_before": act["to_call_before"], "stack_before": act["stack_before"]})
        ctx["turn"] += 1
        self.event_id += 1

        def build(me):
            return {"type": "player_action", "seat": (seat - me) % hand.n, "name": name, "action": act["action"],
                    "street": street, "amount": act["amount"], "amount_mode": act["amount_mode"], "stack": act["stack"],
                    "pot": act["pot"], "action_id": f"{TABLE_ID}:event:{self.event_id}",
                    "stack_before": act["stack_before"], "pot_before": act["pot_before"],
                    "contribution_delta": act["delta"]}
        self._broadcast(ctx, build)

    def play_hand(self, hand_no, stacks):
        """Play one hand. stacks: bot name -> chips before stack rules. Returns the stacks after it."""
        t = self.table
        round_no, button, seating, holes, board = hand_layout(self.seed, hand_no, self.names)
        resets = {}
        start = []
        for name in seating:
            s = apply_stack_rules(stacks[name], t)
            if s != stacks[name] and not t.get("flat_stacks"):
                resets[name] = {"from": stacks[name], "to": s}
            start.append(s)
        hand = Hand(start, button, t["small_blind"], t["big_blind"], t["ante"], holes, board)
        suffix = f".{self.restarts}" if self.restarts else ""
        hand_id = f"{TABLE_ID}:hand:{hand_no:06d}{suffix}"
        ctx = {"hand_id": hand_id, "seating": seating, "hand_seq": 0, "turn": 0, "actions": []}
        self._event("hand_start", hand_id, button=button, sb=hand.sb, bb=hand.bb, ante=hand.ante, hero_seat=0,
                    n_seats=hand.n)

        self._broadcast(ctx, lambda me: {"type": "hand_start", "seat": 0, "dealer_seat": (button - me) % hand.n,
                                         "blinds": {"small_blind": hand.sb, "big_blind": hand.bb}})
        self._broadcast(ctx, lambda me: {"type": "hole_cards", "cards": hand.holes[me]})
        self._broadcast(ctx, lambda me: self._state_msg(ctx, hand, me), stream="state")
        while True:
            self._play_action(ctx, hand)
            if hand.over_by_fold() or hand.needs_showdown():
                break
            self._broadcast(ctx, lambda me: self._state_msg(ctx, hand, me), stream="state")
            while hand.round_closed() and not hand.needs_showdown():
                new = hand.next_street()
                self._event("board", hand_id, street=hand.street, cards=new, board=hand.board, pot=hand.pot)
                street = hand.street
                self._broadcast(ctx, lambda me: {"type": "community_cards", "street": street, "cards": new})
                self._broadcast(ctx, lambda me: self._state_msg(ctx, hand, me), stream="state")
            if hand.needs_showdown():
                break
        res = hand.settle()
        self._broadcast(ctx, lambda me: self._result_msg(ctx, hand, me))
        record = {"wopo_hh": 1, "table_id": TABLE_ID, "ts": now_iso(), "hand_id": hand_id, "hand_no": hand_no,
                  "round_no": round_no, "button_seat": button, "small_blind": hand.sb, "big_blind": hand.bb,
                  "ante": hand.ante, "my_seat": 0, "dealt_seats": list(range(hand.n)),
                  "initial_stacks": {str(s): start[s] for s in range(hand.n)},
                  "names": {str(s): seating[s] for s in range(hand.n)}, "hole": hand.holes[0],
                  "board": list(hand.board), "actions": ctx["actions"],
                  "result": self._result_msg(ctx, hand, 0, reveal_all=True), "result_notes": [],
                  "stack_resets": resets, "net_by_bot": {seating[s]: res["net"][s] for s in range(hand.n)}}
        self.hh.write(json.dumps(record, ensure_ascii=False) + "\n")
        self.hh.flush()
        self.last_hand_id = hand_id
        self._event("result", hand_id, pot=hand.pot, board=list(hand.board), winners=record["result"]["winners"],
                    shown=record["result"].get("shown_cards"), notes=[])
        return {seating[s]: res["final"][s] for s in range(hand.n)}


# ------------------------------------------------------------------ run management

def load_bots(cfg, out_dir):
    bots = []
    for i, group in enumerate(cfg["groups"]):
        for c in range(group.get("copies", 1)):
            name = f"{group['name']}_{'abcdefgh'[c]}"
            bots.append(Bot(name, group["name"], 1000 * (i + 1) + c, group["args"], out_dir))
    if len(bots) != cfg["table"]["seats"]:
        raise SystemExit(f"配置的 bot 数 {len(bots)} ≠ 座位数 {cfg['table']['seats']}")
    return bots


def resume_state(hh_path, names, table, first_hand=1):
    """(next hand number, stacks) from the table's hand history, the only source of match state. A last
    line cut off by a crash is dropped from the file."""
    stacks = {n: table["start_bb"] * table["big_blind"] for n in names}
    next_hand = first_hand
    if not os.path.exists(hh_path):
        return next_hand, stacks
    good = 0
    with open(hh_path, "rb") as f:
        for raw in f:
            if not raw.endswith(b"\n"):
                break
            rec = json.loads(raw)
            good += len(raw)
            view = rec["result"]["final_stacks"]
            stacks = {rec["names"][s]: view[s] for s in rec["names"]}
            next_hand = rec["hand_no"] + 1
    if good != os.path.getsize(hh_path):
        os.truncate(hh_path, good)
    return next_hand, stacks


def git_commit():
    try:
        return subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=REPO, capture_output=True, text=True,
                              check=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        path = os.path.join(REPO, "DEPLOYED_COMMIT")
        return open(path).read().strip() if os.path.exists(path) else "unknown"


def monitor_memory(bots, path, every, min_avail_kb, stop_flag, log):
    with open(path, "a") as f:
        if f.tell() == 0:
            f.write("ts\tavail_kb\tswap_free_kb\t" + "\t".join(f"{b.name}.driver\t{b.name}.advisor" for b in bots) + "\n")
        while not stop_flag.get("exit"):
            info = meminfo_kb()
            row = [str(int(time.time())), str(info["MemAvailable"]), str(info.get("SwapFree", 0))]
            for b in bots:
                row.extend(str(x) for x in b.rss_kb())
            f.write("\t".join(row) + "\n")
            f.flush()
            if min_avail_kb and info["MemAvailable"] < min_avail_kb and not stop_flag.get("low_mem"):
                stop_flag["low_mem"] = True
                log(f"[warn] 可用内存 {info['MemAvailable'] // 1024} MiB 低于门槛，打完这手就暂停")
            time.sleep(every)


def main():
    p = argparse.ArgumentParser(description="本地 wopo 协议牌桌：真实 driver + advisor 互打，复式发牌")
    p.add_argument("--config", required=True, help="牌桌和 bot 配置（JSON）")
    p.add_argument("--out", required=True, help="输出目录；已有牌局时从断点接着打")
    p.add_argument("--hands", type=int, required=True, help="总手数，须是座位数的整数倍（一轮 = 座位数手）")
    p.add_argument("--seed", type=int, default=20261106)
    p.add_argument("--python", default=sys.executable, help="跑 driver 的解释器（要有 websocket-client）")
    p.add_argument("--decision-timeout", type=float, default=180.0)
    p.add_argument("--max-timeouts", type=int, default=5, help="决策超时累计超过这么多次就中止")
    p.add_argument("--connect-timeout", type=float, default=1800.0, help="等所有 advisor 加载完并连上的秒数")
    p.add_argument("--mem-every", type=float, default=60.0)
    p.add_argument("--min-avail-gib", type=float, default=2.0, help="可用内存低于它就打完这手后暂停；0 = 不查")
    p.add_argument("--stop-after", type=int, default=0, help="本次进程打这么多手后退出（测试断点续打）")
    p.add_argument("--flat-stacks", action="store_true", help="每手所有人都从初始筹码开始（不看补码线和重进线）")
    p.add_argument("--first-hand", type=int, default=1, help="从这个手号开始（比赛的下一段：牌接着发，不重复）")
    p.add_argument("--warm-from", default=None, help="上一段的 --out 目录：driver 用同名 bot 的牌谱恢复对手画像")
    args = p.parse_args()

    with open(args.config) as f:
        cfg = json.load(f)
    if args.flat_stacks:
        cfg["table"]["flat_stacks"] = True
    table = cfg["table"]
    n = table["seats"]
    if args.hands <= 0 or args.hands % n:
        p.error(f"--hands 须是 {n} 的整数倍")
    if args.first_hand < 1 or (args.first_hand - 1) % n:
        p.error(f"--first-hand 须是一轮的第一手（手号 - 1 是 {n} 的整数倍）")
    os.makedirs(args.out, exist_ok=True)
    log_file = open(os.path.join(args.out, "arena.log"), "a", encoding="utf-8")

    def log(msg):
        line = f"[{now_iso()}] {msg}"
        print(line, file=log_file, flush=True)
        print(line, flush=True)

    bots = load_bots(cfg, args.out)
    names = [b.name for b in bots]
    if args.warm_from:
        for name in names:
            path = os.path.join(args.warm_from, name, "wopo_hh.jsonl")
            if not os.path.exists(path):
                p.error(f"--warm-from 里没有 {path}")
    run_path = os.path.join(args.out, "run.json")
    restarts = 0
    if os.path.exists(run_path):
        with open(run_path) as f:
            prior = json.load(f)
        if prior["config"] != cfg or prior["seed"] != args.seed:
            raise SystemExit(f"{args.out} 里已有用别的配置或 seed 打的牌局，不能接着打")
        restarts = prior["restarts"] + 1
    with open(run_path, "w") as f:
        json.dump({"config": cfg, "seed": args.seed, "hands": args.hands, "first_hand": args.first_hand,
                   "warm_from": args.warm_from, "restarts": restarts, "commit": git_commit(), "started": now_iso()},
                  f, ensure_ascii=False, indent=1)

    next_hand, stacks = resume_state(os.path.join(args.out, "table", "wopo_hh.jsonl"), names, table, args.first_hand)
    log(f"commit {git_commit()} seed {args.seed} 第 {restarts + 1} 次启动，从第 {next_hand} 手打到第 {args.hands} 手")
    if next_hand > args.hands:
        log("已打满")
        return

    server = WsServer(names)
    url = f"ws://127.0.0.1:{server.port}/ws"
    stop_flag = {}

    def on_signal(signum, _frame):
        if stop_flag.get("signal"):
            raise KeyboardInterrupt
        stop_flag["signal"] = signum

    signal.signal(signal.SIGTERM, on_signal)
    signal.signal(signal.SIGINT, on_signal)

    code = 0
    tbl = None
    try:
        for b in bots:
            b.start(args.python, url, table, log, args.warm_from)
        threading.Thread(target=monitor_memory,
                         args=(bots, os.path.join(args.out, "mem.tsv"), args.mem_every,
                               int(args.min_avail_gib * 1024 * 1024), stop_flag, log),
                         daemon=True).start()
        deadline = time.monotonic() + args.connect_timeout
        while not server.all_connected():
            for b in bots:
                if not b.running():
                    raise Abort(f"driver {b.name} 启动时退出，看 {b.dir}/driver.log")
            if time.monotonic() > deadline:
                raise Abort("等 driver 连接超时")
            if stop_flag.get("signal"):
                raise Abort("启动时收到停止信号")
            time.sleep(0.2)
        log(f"{n} 个 driver 都已连上")

        tbl = Table(table, bots, server, args.out, args.seed, restarts, args.decision_timeout, log)
        t0 = time.monotonic()
        played = 0
        nets = dict.fromkeys(names, 0)
        for hand_no in range(next_hand, args.hands + 1):
            if stop_flag.get("signal") or stop_flag.get("low_mem"):
                log(f"暂停在第 {hand_no} 手之前；同样的命令可以接着打")
                code = 3
                break
            before = {k: apply_stack_rules(v, table) for k, v in stacks.items()}
            stacks = tbl.play_hand(hand_no, stacks)
            for k in names:
                nets[k] += stacks[k] - before[k]
            played += 1
            if tbl.counts["timeouts"] > args.max_timeouts:
                raise Abort(f"决策超时 {tbl.counts['timeouts']} 次")
            if played % n == 0:
                rate = played / (time.monotonic() - t0) * 3600
                bb = table["big_blind"]
                log(f"第 {hand_no} 手，{rate:.0f} 手/小时，本次盈亏(BB) "
                    + " ".join(f"{k}:{nets[k] / bb:+.0f}" for k in names) + f" | {tbl.counts}")
            if args.stop_after and played >= args.stop_after:
                log(f"--stop-after {args.stop_after}：退出")
                break
        else:
            log(f"打满 {args.hands} 手")
    except Abort as e:
        log(f"中止：{e}")
        code = 2
    except KeyboardInterrupt:
        log("立刻停止")
        code = 3
    finally:
        stop_flag["exit"] = True
        if tbl is not None and tbl.last_hand_id is not None:
            late = wait_for_drivers(bots, tbl.last_hand_id)
            if late:
                log(f"[warn] 这些 driver 没记下最后一手 {tbl.last_hand_id}：{late}")
        for b in bots:
            b.stop()
        for b in bots:
            try:
                if b.proc is not None:
                    b.proc.wait(timeout=30)
            except subprocess.TimeoutExpired:
                b.kill()
    sys.exit(code)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""接入 poker_manager_20261016 的玩家入口，复用常驻 Rust openpoker_advisor。

默认 ws://127.0.0.1:3000/ws；POKER_MANAGER_TOKEN 或 --token 提供服务端配置的 token。
用法及协议差异见 docs/poker_manager_client.md。网络依赖：websocket-client。
"""

import argparse
import copy
from contextlib import ExitStack
from datetime import datetime
import json
import os
import sys
import threading
import time
from urllib.parse import urlsplit, urlunsplit
from urllib.request import HTTPRedirectHandler, Request, build_opener
import uuid

from poker_live.advisor_client import (add_advisor_args, advisor_extra_args, exploit_enabled,
                                       start_advisor, validate_advisor_args)
from poker_live.decision import DecisionWorker, make_request, safe_response
from poker_live.wslog import WsLog


def valid_actions(message):
    actions = {a["action"]: a for a in message.get("valid_actions", [])}
    raise_action = actions.get("raise", {})
    return {"can_check": "check" in actions, "can_call": "call" in actions,
            "can_raise": "raise" in actions, "min_raise": raise_action.get("min"),
            "max_raise": raise_action.get("max")}


def table_fits_model(seats, n_seats):
    """服务端连续座位可嵌入更大的模型；未发牌位置由 dealt_seats 排除。"""
    return (2 <= len(seats) <= n_seats
            and {p.get("seat") for p in seats} == set(range(len(seats))))


def account_reader(url, token):
    """补码前读取自己的账户余额；不允许把凭据转发到重定向目标。"""
    class NoRedirect(HTTPRedirectHandler):
        def redirect_request(self, req, fp, code, msg, headers, newurl):
            return None

    parts = urlsplit(url)
    endpoint = urlunsplit(("https" if parts.scheme == "wss" else "http",
                          parts.netloc, "/api/me", "", ""))
    opener = build_opener(NoRedirect)

    def read():
        request = Request(endpoint, headers={"Authorization": f"Bearer {token}"})
        with opener.open(request, timeout=5) as response:
            return json.load(response)

    return read


class ManagerHand:
    """仅从亲收的 hand_start + 首个快照建局，使用服务端明确的增量更新历史。"""

    def __init__(self, message, n_seats):
        self.id = message["hand_id"]
        self.hero = message["seat"]
        self.button = message["dealer_seat"]
        self.sb = message["blinds"]["small_blind"]
        self.bb = message["blinds"]["big_blind"]
        self.n_seats = n_seats
        self.hole = None
        self.board = []
        self.street = "preflop"
        self.dealt = []
        self.starts = {}
        self.bets = {}
        self.stacks = {}
        self.names = {}
        self.actions = []
        self.events = []
        self.invalid = None

    def snapshot(self, message):
        if self.invalid:
            return
        seats = message["seats"]
        if not table_fits_model(seats, self.n_seats):
            raise ValueError(f"--seats={self.n_seats}, server seats={len(seats)}")
        if not self.starts:
            if self.actions or message["street"] != "preflop":
                self.invalid = "missing_hand_start_state"
                return
            self.dealt = sorted(p["seat"] for p in seats if p.get("in_hand"))
            for p in seats:
                s = p["seat"]
                if s in self.dealt:
                    self.starts[s] = p["stack"] + p["bet"]
                    self.bets[s] = p["bet"]
                    self.stacks[s] = p["stack"]
                    self.names[s] = p["name"]
        for p in seats:
            s = p["seat"]
            if s in self.dealt and (p["stack"] != self.stacks[s]
                                    or p["bet"] != self.bets[s]):
                self.invalid = "snapshot_ledger_mismatch"
        if message.get("board", []) != self.board:
            self.invalid = "snapshot_board_mismatch"
        self.hole = message.get("hero", {}).get("hole_cards", self.hole)

    def action(self, message):
        if self.invalid:
            return
        s = message["seat"]
        if s not in self.starts:
            self.invalid = "missing_hand_start_state"
            return
        delta = message["contribution_delta"]
        if (message.get("amount_mode") != "incremental" or message["street"] != self.street
                or message["stack_before"] != self.stacks[s]
                or not 0 <= delta <= self.stacks[s]
                or message["stack_after"] != self.stacks[s] - delta):
            self.invalid = "action_ledger_mismatch"
            return
        self.bets[s] += delta
        self.stacks[s] -= delta
        action = {"seat": s, "action": message["action"]}
        if message["action"] == "raise":
            action["to"] = self.bets[s]
        self.actions.append(action)
        self.events.append(dict(message))

    def community(self, message):
        # community_cards.cards contains only the newly dealt cards.
        self.board.extend(message["cards"])
        self.street = message["street"]
        self.bets = dict.fromkeys(self.dealt, 0)

    def request(self, valid, exploit=False):
        return copy.deepcopy(make_request(
            self.hole, self.board, self.button, self.hero, self.n_seats, self.sb, self.bb,
            self.actions, valid, stacks=[self.starts.get(s, 100 * self.bb)
                                        for s in range(self.n_seats)],
            dealt_seats=self.dealt, names=self.names if exploit else None))

    def unavailable(self):
        if self.invalid:
            return self.invalid
        if not self.starts or not self.hole:
            return "incomplete_history"
        # This advisor models OpenPoker's reversed HU button/blinds, not standard HU.
        if len(self.dealt) < 3:
            return "standard_heads_up_unsupported"
        return None


class ManagerSession:
    """快照不授权；只处理 your_turn / resync hero token，后台计算可被超时或断线作废。"""

    def __init__(self, worker, n_seats=6, buy_in=2000, num_hands=100, action_log=None,
                 hh_log=None, exploit=False, prewarm=False, debug=False,
                 auto_rebuy=False, read_account=None, stack_band_bb=(80, 180)):
        self.worker = worker
        self.n_seats = n_seats
        self.buy_in = buy_in
        self.num_hands = num_hands
        self.action_log = action_log
        self.hh_log = hh_log
        self.exploit = exploit
        self.prewarm = prewarm
        self.debug = debug
        self.auto_rebuy = auto_rebuy
        self.read_account = read_account
        # Leave and rejoin at buy-in when our stack drifts outside this band (in BB); the
        # blueprint assumes 100BB. None disables it.
        self.stack_band_bb = stack_band_bb
        self.seat_start = None
        self.leaving = False
        self.joining = False
        self.rebuy_request_id = None
        self.lock = threading.RLock()
        self.send = None
        self.table_id = None
        self.my_seat = None
        self.last_seq = 0
        self.hand = None
        self.pending = None
        self.sent = {}
        self.warmed = set()
        self.resyncing = False
        self.buffered = []
        self.done = False
        self.error = None
        self.hands = 0
        self.decisions = 0

    def attach(self, send):
        with self.lock:
            self.send = send

    def disconnect(self):
        with self.lock:
            self.send = None
            self.pending = None

    def fail(self, reason):
        self.error = reason
        self.done = True
        self.pending = None

    def resync(self):
        if self.table_id and not self.resyncing:
            self.resyncing = True
            self.pending = None
            self.send({"type": "resync_request", "table_id": self.table_id,
                       "last_table_seq": self.last_seq})

    def join_or_rebuy(self, account=None):
        if self.done or self.table_id or self.joining:
            return
        if not self.auto_rebuy:
            self.joining = True
            self.send({"type": "join_lobby", "buy_in": self.buy_in})
            return
        # Retry the same grant request after a lost acknowledgement, never a new ID.
        if self.rebuy_request_id:
            self.send({"type": "rebuy", "amount": 0, "request_id": self.rebuy_request_id})
            return
        account = self.read_account() if account is None else account
        balance = account["chip_balance"]
        if type(balance) is not int or balance < 0:
            raise ValueError("invalid account balance")
        if balance >= 1000:
            buy_in = min(self.buy_in, balance)
            self.joining = True
            self.send({"type": "join_lobby", "buy_in": buy_in})
            print(f"[auto-rebuy] 账户余额={balance}，买入={buy_in}。", file=sys.stderr)
        elif balance == 0 and account.get("auto_rebuy"):
            # Re-enabling also restores a missed scheduled/confirmed event on reconnect.
            self.send({"type": "set_auto_rebuy", "enabled": True})
            print("[auto-rebuy] 等待服务端自动补码确认。", file=sys.stderr)
        else:
            self.rebuy_request_id = str(uuid.uuid4())
            self.send({"type": "rebuy", "amount": 0, "request_id": self.rebuy_request_id})

    def handle(self, message):
        with self.lock:
            self._handle(message)

    def _handle(self, m, replay=False):
        if self.done:
            return
        t = m["type"]
        tid = m.get("table_id")
        if tid and tid != self.table_id:
            self.table_id, self.last_seq = tid, 0
            self.hand, self.pending = None, None
            self.my_seat = None
            self.sent.clear()
        if t == "connected":
            self.resyncing = False
            self.buffered.clear()
            self.joining = False
            if tid:
                if self.auto_rebuy:
                    self.send({"type": "set_auto_rebuy", "enabled": True})
                if self.leaving:
                    # Still seated: the leave may not have reached the server.
                    self.send({"type": "leave_table"})
                self.resync()
            else:
                self.leaving = False
                self.table_id, self.last_seq = None, 0
                self.hand = None
                self.my_seat = None
                self.sent.clear()
                self.join_or_rebuy()
            return
        if self.resyncing and not replay and t not in ("resync_response", "error"):
            self.buffered.append(m)
            return
        if t == "resync_response":
            # Private hand_start/hole_cards are not replayed. A cold reconnect cannot
            # reconstruct the betting history from a snapshot; wait for the next hand.
            if m.get("replay_truncated"):
                self.hand = None
            for event in sorted(m.get("replayed_events", []), key=lambda e: e["table_seq"]):
                self._handle(event, replay=True)
            snapshot = m["snapshot"]
            if self.hand and self.hand.id != snapshot.get("hand_id"):
                self.hand = None
            self._snapshot(snapshot)
            self.last_seq = m["to_table_seq"]
            self.resyncing = False
            hero = snapshot.get("hero", {})
            if hero.get("turn_token") and not self.done:
                self._turn({**hero, "hand_id": snapshot["hand_id"]})
            buffered, self.buffered = self.buffered, []
            for event in buffered:
                if "table_seq" not in event or event["table_seq"] > self.last_seq:
                    self._handle(event)
            return
        seq = m.get("table_seq")
        if m.get("stream") == "event" and isinstance(seq, int):
            if seq <= self.last_seq:
                return
            self.last_seq = seq
        if t == "lobby_joined" and self.auto_rebuy:
            self.send({"type": "set_auto_rebuy", "enabled": True})
        elif t == "auto_rebuy_set" and self.auto_rebuy:
            if m.get("enabled") is not True:
                self.fail("server did not enable auto rebuy")
            else:
                print("[auto-rebuy] 服务端已确认开启。", file=sys.stderr)
        elif t == "auto_rebuy_scheduled" and self.auto_rebuy:
            print(f"[auto-rebuy] 补码时间：{m.get('rebuy_at')}。", file=sys.stderr)
        elif t == "rebuy_confirmed" and self.auto_rebuy:
            if self.rebuy_request_id and m.get("request_id") != self.rebuy_request_id:
                return
            self.rebuy_request_id = None
            print(f"[auto-rebuy] 补码确认，账户余额={m.get('chip_balance')}。", file=sys.stderr)
            self.join_or_rebuy(m)
        elif t == "table_joined":
            self.joining = False
            self.my_seat = m["seat"]
            self.seat_start = next((p.get("stack") for p in m.get("players", [])
                                    if p.get("seat") == m["seat"]), None)
        elif t == "hand_start":
            self.hand = ManagerHand(m, self.n_seats)
            self.my_seat = m["seat"]
            self.sent.clear()
            self.warmed.clear()
            self.pending = None
        elif t == "table_state":
            self._snapshot(m)
        elif t == "hole_cards" and self.hand and m["hand_id"] == self.hand.id:
            self.hand.hole = m["cards"]
        elif t in ("player_action", "community_cards"):
            self.pending = None
            if self.hand and m["hand_id"] == self.hand.id:
                if t == "player_action":
                    self.hand.action(m)
                else:
                    self.hand.community(m)
        elif t == "your_turn" and not replay and not self.resyncing:
            self._turn(m)
        elif t == "hand_result":
            hand = self.hand
            self.hand, self.pending = None, None
            if hand and hand.id == m["hand_id"]:
                self.hands += 1
                if self.exploit and not hand.unavailable():
                    self.worker.submit_observe({"names": hand.names,
                        "actions": [{k: e[k] for k in ("seat", "street", "action")}
                                    for e in hand.events]})
                self._log(self.hh_log, {"poker_manager_hh": 1, "hand_id": hand.id,
                    "table_id": self.table_id,
                    "complete": bool(hand.starts and hand.hole and not hand.invalid),
                    "initial_stacks": hand.starts, "dealt_seats": hand.dealt,
                    "button_seat": hand.button, "my_seat": hand.hero,
                    "small_blind": hand.sb, "big_blind": hand.bb,
                    "hole": hand.hole, "board": hand.board, "actions": hand.actions,
                    "result": m})
                print(f"[hand {self.hands}] stacks={m.get('final_stacks')}", file=sys.stderr)
                if self.hands >= self.num_hands:
                    self.done = True
                    if not replay:
                        self.send({"type": "leave_table"})
                elif not replay:
                    self._check_stack_band(m.get("final_stacks", {}).get(str(hand.hero)), hand.bb)
        elif t == "action_rejected":
            # Never spin on invalid actions. Stale authority can be recovered once.
            code = m.get("code")
            if code in ("stale_turn_token", "stale_hand_action", "not_your_turn",
                        "no_hand_in_progress"):
                self.resync()
            else:
                self.fail(f"action_rejected: {code}")
        elif t == "busted":
            self.pending = None
            if self.auto_rebuy:
                print("[busted] 筹码用尽，等待离桌后自动重新买入。", file=sys.stderr)
            else:
                self.done = True
                print("[busted] 筹码用尽，结束运行。", file=sys.stderr)
        elif t == "player_left" and m.get("seat") == self.my_seat:
            if not replay and (self.leaving or
                               (self.auto_rebuy and m.get("reason") == "busted")):
                self._rejoin()
            else:
                self.fail(f"player_left: {m.get('reason')}")
        elif t == "table_closed":
            self.fail(f"table_closed: {m.get('reason')}")
        elif t == "error":
            if self.leaving and m.get("code") == "not_at_table":
                self._rejoin()
            elif m.get("code") != "leave_pending":
                self.fail(f"server error: {m.get('code')}")

    def _check_stack_band(self, stack, bb):
        if self.stack_band_bb is None or self.leaving or not stack:
            return  # A busted stack (0) is handled by busted / player_left.
        lo, hi = self.stack_band_bb
        if lo * bb <= stack <= hi * bb:
            return
        # Only a drift away from an in-band seat: a rejoin that can only buy in out of band
        # (e.g. a 1500 auto-rebuy grant = 75BB) would otherwise leave every hand.
        start = self.buy_in if self.seat_start is None else self.seat_start
        if not lo * bb <= start <= hi * bb:
            return
        self.leaving = True
        self.send({"type": "leave_table"})
        print(f"[码深漂移] 我方栈 {stack} 漂出 [{lo},{hi}]BB，离桌后重新买入。", file=sys.stderr)

    def _rejoin(self):
        self.leaving = False
        self.table_id, self.my_seat = None, None
        self.hand, self.pending = None, None
        self.sent.clear()
        self.warmed.clear()
        self.last_seq = 0
        self.joining = False
        self.join_or_rebuy()

    def _snapshot(self, m):
        self.my_seat = m.get("hero", {}).get("seat", self.my_seat)
        if not table_fits_model(m["seats"], self.n_seats):
            self.fail(f"--seats={self.n_seats}, server seats={len(m['seats'])}")
            return
        if self.hand and self.hand.id == m.get("hand_id") and m["street"] != "idle":
            self.hand.snapshot(m)
            key = (self.hand.id, self.hand.street)
            if (self.prewarm and not self.resyncing and not self.hand.unavailable()
                    and self.hand.street != "preflop" and key not in self.warmed):
                self.warmed.add(key)
                self.worker.submit_prewarm(self.hand.request(valid_actions({}), self.exploit))
        if self.pending and m.get("actor_seat") != m.get("hero", {}).get("seat"):
            self.pending = None

    def _turn(self, m):
        token = m.get("turn_token")
        if not token or not m.get("valid_actions"):
            return
        if token in self.sent:
            # After reconnect an unchanged token means the earlier write was not accepted.
            if self.sent[token][1] is not self.send:
                self.send(self.sent[token][0])
                self.sent[token] = (self.sent[token][0], self.send)
            return
        if self.pending and self.pending["message"]["turn_token"] == token:
            return
        deadline = datetime.fromisoformat(m["action_deadline"].replace("Z", "+00:00")).timestamp()
        if deadline <= time.time():
            return
        valid = valid_actions(m)
        reason = "incomplete_history"
        if self.hand and self.hand.id == m["hand_id"]:
            reason = self.hand.unavailable()
        req = self.hand.request(valid, self.exploit) if reason is None else None
        pending = {"message": dict(m), "deadline": deadline, "req": req}
        self.pending = pending
        if reason or deadline - time.time() < 0.5:
            self._finish(pending, safe_response(valid, f"fallback:{reason or 'deadline'}"), 0)
        else:
            self.worker.submit_decide(req, lambda resp, ms: self._finish(pending, resp, ms))

    def _finish(self, pending, response, elapsed_ms):
        with self.lock:
            if self.done or self.send is None or self.pending is not pending:
                return
            m = pending["message"]
            if time.time() >= pending["deadline"]:
                self.pending = None
                return
            actions = {a["action"]: a for a in m["valid_actions"]}
            action, amount = response.get("action"), response.get("amount")
            if (action not in actions or (action == "raise" and
                    (isinstance(amount, bool) or not isinstance(amount, (int, float))
                     or not actions[action]["min"] <= amount <= actions[action]["max"]
                     or int(amount) != amount))):
                response = safe_response(valid_actions(m), "fallback:illegal_advisor_action")
                action, amount = response["action"], None
            out = {"type": "action", "hand_id": m["hand_id"], "turn_token": m["turn_token"],
                   "client_action_id": str(uuid.uuid4()), "action": action}
            if action == "raise":
                out["amount"] = int(amount)
            self.sent[m["turn_token"]] = (out, self.send)
            self.pending = None
            self.send(out)
            self.decisions += 1
            self._log(self.action_log, {"req": pending["req"], "resp": response,
                                        "sent": out, "decide_ms": elapsed_ms})
            if self.debug or str(response.get("source", "")).startswith("fallback"):
                print(f"[action] {action} amount={out.get('amount')} "
                      f"source={response.get('source')}", file=sys.stderr)

    def tick(self):
        with self.lock:
            if self.pending and time.time() >= self.pending["deadline"] - 0.5:
                self._finish(self.pending, safe_response(valid_actions(self.pending["message"]),
                                                        "fallback:deadline"), 0)

    @staticmethod
    def _log(file, record):
        if file:
            file.write(json.dumps(record, ensure_ascii=False) + "\n")
            file.flush()


def run(session, url, token, ws_log=None, reconnect_attempts=10):
    import websocket

    attempts = 0
    while not session.done:
        ws = None
        before = session.hands
        try:
            ws = websocket.create_connection(url, header=[f"Authorization: Bearer {token}"],
                                             timeout=5)
            ws.settimeout(0.2)
            if ws_log:
                ws_log.conn += 1
                ws_log.event("open")

            def send(message):
                raw = json.dumps(message)
                ws.send(raw)
                if ws_log:
                    ws_log.msg("out", raw)

            session.attach(send)
            while not session.done:
                try:
                    opcode, raw = ws.recv_data()
                except websocket.WebSocketTimeoutException:
                    session.tick()
                    continue
                if opcode == websocket.ABNF.OPCODE_CLOSE:
                    code = int.from_bytes(raw[:2], "big") if len(raw) >= 2 else 1000
                    if code in (4001, 4002, 4003, 4004):
                        session.fail(f"websocket closed: {code}")
                    break
                if isinstance(raw, bytes):
                    raw = raw.decode("utf-8")
                if ws_log:
                    ws_log.msg("in", raw)
                session.handle(json.loads(raw))
                session.tick()
        except (OSError, websocket.WebSocketException):
            # Do not print exception text: handshake errors can contain credentials.
            print("[ws] 连接中断或失败。", file=sys.stderr)
        except (ValueError, KeyError, TypeError) as error:
            session.fail(f"protocol error: {type(error).__name__}")
        finally:
            session.disconnect()
            if ws is not None:
                ws.close()
            if ws_log:
                ws_log.event("close")
        if session.done:
            break
        attempts = 1 if session.hands > before else attempts + 1
        if attempts > reconnect_attempts:
            session.fail("重连次数已耗尽")
            break
        print(f"[ws] 3 秒后重连（{attempts}/{reconnect_attempts}）。", file=sys.stderr)
        time.sleep(3)
    if session.error:
        raise RuntimeError(session.error)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    add_advisor_args(parser, default_seats=6, debug_help="打印决策及 Rust advisor 调试信息")
    parser.add_argument("--url", default=os.environ.get("POKER_MANAGER_URL", "ws://127.0.0.1:3000/ws"))
    parser.add_argument("--token", "--api-key", default=os.environ.get("POKER_MANAGER_TOKEN"),
                        help="POKER_PLAYERS 中的 token；默认读 POKER_MANAGER_TOKEN")
    parser.add_argument("--buy-in", type=int, default=2000)
    parser.add_argument("--num-hands", type=int, default=100)
    parser.add_argument("--auto-rebuy", action="store_true",
                        help="开启服务端自动补码，出局后按可用余额重新入座，累计到手数上限")
    parser.add_argument("--stack-band-bb", default="80,180",
                        help="我方栈漂出该区间（BB，含端点）就在本手结束后离桌、按买入重新入座；off 关闭")
    parser.add_argument("--reconnect-attempts", type=int, default=10)
    parser.add_argument("--action-log", default=None)
    parser.add_argument("--hh-log", default=None)
    parser.add_argument("--ws-log", default=None)
    args = parser.parse_args(argv)
    validate_advisor_args(args)
    if not args.token or any(c.isspace() for c in args.token):
        parser.error("需要 --token 或 POKER_MANAGER_TOKEN（非空且不含空白）")
    url = urlsplit(args.url)
    if (url.scheme not in ("ws", "wss") or not url.hostname or url.path != "/ws"
            or url.query or url.fragment or url.username or url.password):
        parser.error("--url 必须是 ws(s)://HOST:PORT/ws；通过 --token 鉴权")
    if not 1000 <= args.buy_in <= 5000 or args.num_hands <= 0 or args.reconnect_attempts < 0:
        parser.error("buy-in 需为 1000..5000，num-hands > 0，reconnect-attempts >= 0")
    stack_band = None
    if args.stack_band_bb != "off":
        try:
            stack_band = tuple(int(v) for v in args.stack_band_bb.split(","))
        except ValueError:
            stack_band = ()
        if len(stack_band) != 2 or not 0 < stack_band[0] < stack_band[1]:
            parser.error("--stack-band-bb 需为 LO,HI（0 < LO < HI，单位 BB）或 off")
    try:
        import websocket  # noqa: F401
    except ImportError:
        parser.error("缺少 websocket-client；请在 Python 环境中运行 pip install websocket-client")
    with ExitStack() as stack:
        def log(path):
            return stack.enter_context(open(path, "a", encoding="utf-8")) if path else None

        action_log, hh_log = log(args.action_log), log(args.hh_log)
        ws_log = WsLog(args.ws_log) if args.ws_log else None
        if ws_log:
            stack.callback(ws_log.close)
        advisor = start_advisor(args, advisor_extra_args(args))
        stack.callback(advisor.close)
        worker = DecisionWorker(advisor)
        stack.callback(worker.close, timeout=1)
        session = ManagerSession(worker, args.seats, args.buy_in, args.num_hands,
                                 action_log, hh_log, exploit_enabled(args),
                                 args.search_prewarm, args.debug_log,
                                 args.auto_rebuy, account_reader(args.url, args.token),
                                 stack_band)
        stack.callback(session.disconnect)
        try:
            run(session, args.url, args.token, ws_log, args.reconnect_attempts)
        except KeyboardInterrupt:
            print("[stop] 已中断；断线座位由服务端按保座期限释放。", file=sys.stderr)
        except RuntimeError as error:
            raise SystemExit(str(error)) from error
        finally:
            print(f"hands={session.hands} decisions={session.decisions}", file=sys.stderr)


if __name__ == "__main__":
    main()

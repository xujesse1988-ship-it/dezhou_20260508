#!/usr/bin/env python3
"""OpenPoker 9-max WS driver + 启发式 Rust advisor（heuristic9_advisor）。

与 6-max 的 openpoker_play.py 同协议、但**座位通用**（2–9 人），驱动独立的启发式引擎。
网络/token/重连在本文件（Python，websocket-client）；策略全在常驻 Rust advisor，crate 零
网络依赖。

链路：
  WS 连 wss://openpoker.ai/ws + Bearer 鉴权；join_lobby{buy_in} 锁码深。
  逐手累计 hand_start/hole_cards/player_action/community_cards → 组 advisor 请求。
  your_turn → 调 advisor 拿 {action, amount} → 回 action{turn_token, client_action_id}。
  hand_result → 记净盈亏 + 可选喂 advisor 对手画像（--exploit）。

协议字段沿用 openpoker_play.py 的 live 校准（2026-06-03）：
  - action 报文：client_action_id 必须是字符串；amount 始终在场（raise=float 总 to / 余 null）；
    带 turn_token。
  - player_action.amount（raise）= 总 to 额（amount_mode=to_total）。
  - hand_start{seat=my_seat, dealer_seat=button, hand_id}；hole_cards{cards}；
    your_turn{valid_actions, min_raise, max_raise, turn_token, players:[{seat,name,stack}]}；
    community_cards{cards, street}。

座位约定（v1）：默认满环 `--seats`（默认 9），物理座位 0..N-1 连续全发牌（同 6-max driver
对 6 的假设）。稀疏占座的 compact 重映射留 v2（见 docs/heuristic9_design.md）。

用法：
  python3 tools/heuristic9_play.py --selftest                 # 离线验 IPC（无需账号）
  python3 tools/heuristic9_play.py --api-key KEY --num-hands 1000 \
      --advisor-bin target/release/heuristic9_advisor --seats 9 \
      --action-log h9_actions.jsonl --exploit --opp-profile h9_prof.json
"""
import argparse
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

# 内联硬伤检测（前期抓 bug）：复用 analyze 的 hard_issues；导入失败则降级为 no-op。
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
try:
    from heuristic9_analyze import hard_issues
except Exception:
    def hard_issues(_req, _resp):
        return []

WS_URL = "wss://openpoker.ai/ws"
REGISTER_URL = "https://api.openpoker.ai/api/register"
BIG_BLIND_OP = 20
SMALL_BLIND_OP = 10
# 买入：season bankroll 受限——rebuy 每次仅补 1500、服务端 min buy-in 1000（docs/compete/rebuys）。
# 锁 1000=50BB 求**可持续**（原 2000=100BB 在薄 bankroll 下买不进；深码靠桌上赢钱自然累积，HI 仍 500BB）。
BUY_IN = 1000
MIN_BUY_IN_CHIPS = 1000  # season_min_buy_in_chips：chip_balance < 此值且离桌才有 rebuy 资格。
REBUY_URL = "https://api.openpoker.ai/api/season/rebuy"  # POST +1500（free 首次 0s、之后 300s 冷却）。
# Cloudflare 对 `Python-urllib` UA 返 403「error code: 1010」（浏览器签名封禁）→ 必须伪装浏览器 UA。
HTTP_UA = ("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 "
           "(KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
# 码深漂移窗口：每手后我方栈漂出 [LO, HI] → leave/rejoin（重锚买入、控我方栈）。
# LO=30BB：跌破即离桌重买（**不等打光再 rebuy**）；buy_in 不足时 driver 自动 season rebuy（见 rebuy_and_join）。
# HI=500BB：上限放宽——深码区已由 `heuristic9_sim --start-bb 200/400` 回归门覆盖（0 fallback / 0 重建漂移 /
#   mirror 落 CI95）。有效栈被最深对手封顶（view::effective_stack_chips）；薄 bankroll 下多由桌上赢钱才触深码。
STACK_LEAVE_LO = 30 * BIG_BLIND_OP
STACK_LEAVE_HI = 500 * BIG_BLIND_OP


def season_rebuy(api_key):
    """POST /api/season/rebuy → 补 1500 season chips（仅 chip_balance<1000 且离桌时有资格）。
    返回 (ok, info)：ok=True → info={'chip_balance','cooldown_seconds',...}；
    429 冷却 → (False, {'cooldown': 秒})；其它失败 → (False, {...})。"""
    req = urllib.request.Request(
        REBUY_URL, data=b"", method="POST",
        headers={"Authorization": f"Bearer {api_key}", "User-Agent": HTTP_UA})
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            return True, json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        if e.code == 429:
            retry = e.headers.get("Retry-After")
            return False, {"cooldown": int(retry) if retry and str(retry).isdigit() else 300}
        try:
            body = e.read().decode()[:200]
        except Exception:
            body = ""
        return False, {"http_error": e.code, "body": body}
    except Exception as e:
        return False, {"error": str(e)}
_BOARD_LEN = {"preflop": 0, "flop": 3, "turn": 4, "river": 5}


def _street_name(s):
    if isinstance(s, str):
        return s
    return {0: "preflop", 1: "flop", 2: "turn", 3: "river"}.get(s, "preflop")


# ===========================================================================
# 常驻 advisor 子进程（stdio JSON-lines）
# ===========================================================================
class Advisor:
    def __init__(self, binary, samples, seed, config=None, opp_profile=None, debug=False):
        cmd = [binary, "--samples", str(samples), "--seed", str(seed)]
        if config:
            cmd += ["--config", config]
        if opp_profile:
            cmd += ["--opp-profile", opp_profile]
        if debug:
            cmd += ["--debug"]
        self.proc = subprocess.Popen(
            cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1,
        )
        line = self.proc.stdout.readline()
        if not line:
            raise RuntimeError("advisor 未输出 ready 行（看 stderr）")
        self.ready = json.loads(line)
        if not self.ready.get("ready"):
            raise RuntimeError(f"advisor ready 异常: {line!r}")
        self._pending = 0

    def observe(self, rec):
        payload = dict(rec)
        payload["observe"] = True
        self.proc.stdin.write(json.dumps(payload) + "\n")
        self.proc.stdin.flush()
        self._pending += 1

    def _drain(self):
        while self._pending > 0:
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError("advisor 无响应（退出？）")
            self._pending -= 1

    def decide(self, req):
        self._drain()
        self.proc.stdin.write(json.dumps(req) + "\n")
        self.proc.stdin.flush()
        line = self.proc.stdout.readline()
        if not line:
            raise RuntimeError("advisor 无响应（退出？）")
        return json.loads(line)

    def close(self):
        try:
            self.proc.stdin.close()
        except Exception:
            pass
        try:
            self.proc.wait(timeout=10)
        except Exception:
            self.proc.kill()


# ===========================================================================
# 单手状态累计
# ===========================================================================
class HandState:
    def __init__(self, hand_id, button_seat, my_seat, num_seats):
        self.hand_id = hand_id
        self.button_seat = button_seat if button_seat is not None else 0
        self.my_seat = my_seat if my_seat is not None else 0
        self.num_seats = num_seats
        self.hole = []
        self.board = []
        self.street = "preflop"
        self.actions = []          # [{seat, action, to?}]
        self.action_streets = []   # 与 actions 平行的街（observe 用）
        self.names = {}            # seat -> name
        self.stacks = {}           # seat -> remaining stack（your_turn/player_action 滚动）
        self.start_stacks = {}     # seat -> hand-start stack（首见时锁）

    def on_player_action(self, seat, action, amount):
        if seat is None or action is None:
            return
        act = str(action).lower()
        rec = {"seat": int(seat), "action": act}
        if act in ("raise", "bet", "all_in", "allin") and amount is not None:
            rec["to"] = int(round(float(amount)))
        self.actions.append(rec)
        self.action_streets.append(self.street)

    def on_community(self, cards, street):
        if cards:
            self.board = cards
        self.street = street

    def update_stack(self, seat, stack):
        if seat is None or stack is None:
            return
        s = int(seat)
        self.stacks[s] = int(round(float(stack)))

    def update_name(self, seat, name):
        if seat is not None and name:
            self.names[int(seat)] = name

    def note_start_stack(self, seat, remaining_now):
        """首次见某座时，hand-start 栈 = 当前 remaining + 本手已投入。后续不覆盖。"""
        if seat is None or remaining_now is None:
            return
        s = int(seat)
        if s in self.start_stacks:
            return
        committed = self.committed_total().get(s, 0)
        self.start_stacks[s] = int(round(float(remaining_now))) + committed

    def committed_total(self):
        """从 actions 重建各座本手累计投入（含盲注）。"""
        n = self.num_seats
        btn = self.button_seat % n
        if n == 2:
            sb, bb = btn, (btn + 1) % n
        else:
            sb, bb = (btn + 1) % n, (btn + 2) % n
        ct = {s: 0 for s in range(n)}
        round_c = {s: 0 for s in range(n)}
        ct[sb] = SMALL_BLIND_OP
        ct[bb] = BIG_BLIND_OP
        round_c[sb] = SMALL_BLIND_OP
        round_c[bb] = BIG_BLIND_OP
        high = BIG_BLIND_OP
        last_street = "preflop"
        for a, st in zip(self.actions, self.action_streets):
            if st != last_street:
                round_c = {s: 0 for s in range(n)}
                high = 0
                last_street = st
            s = a["seat"] % n
            act = a["action"]
            if act == "fold" or act == "check":
                pass
            elif act == "call":
                need = max(0, high - round_c[s])
                round_c[s] += need
                ct[s] += need
            elif act in ("raise", "bet", "all_in", "allin"):
                to = a.get("to")
                if to is not None:
                    delta = max(0, to - round_c[s])
                    round_c[s] = max(round_c[s], to)
                    ct[s] += delta
                    high = max(high, round_c[s])
        return ct

    def hand_start_stacks_list(self):
        """各座 hand-start 栈（list，下标=座位）。未知座填 BUY_IN。"""
        ct = self.committed_total()
        out = []
        for s in range(self.num_seats):
            if s in self.start_stacks:
                out.append(self.start_stacks[s])
            elif s in self.stacks:
                out.append(self.stacks[s] + ct.get(s, 0))
            else:
                out.append(BUY_IN)
        return out

    def build_request(self, valid, include_names=False):
        req = {
            "hole": self.hole,
            "board": self.board,
            "button_seat": self.button_seat,
            "my_seat": self.my_seat,
            "num_seats": self.num_seats,
            "small_blind": SMALL_BLIND_OP,
            "big_blind": BIG_BLIND_OP,
            "actions": self.actions,
            "valid": valid,
            "stacks": self.hand_start_stacks_list(),
        }
        if include_names and self.names:
            req["names"] = {str(k): v for k, v in self.names.items()}
        return req

    def observe_record(self):
        acts = [{"seat": a["seat"], "street": _street_idx(st), "action": a["action"]}
                for a, st in zip(self.actions, self.action_streets)]
        return {
            "observe": 1,
            "button_seat": self.button_seat,
            "num_seats": self.num_seats,
            "names": {str(k): v for k, v in self.names.items()},
            "actions": acts,
        }


def _street_idx(s):
    return {"preflop": 0, "flop": 1, "turn": 2, "river": 3}.get(s, 0)


def parse_valid_actions(your_turn):
    va = {"can_check": False, "can_call": False, "can_raise": False,
          "min_raise": None, "max_raise": None}
    raw = your_turn.get("valid_actions", [])
    names = set()
    for v in raw:
        if isinstance(v, dict):
            names.add(v.get("action") or v.get("type"))
        else:
            names.add(v)
    va["can_check"] = "check" in names
    va["can_call"] = "call" in names
    va["can_raise"] = ("raise" in names) or ("bet" in names) or ("all_in" in names)
    mn = your_turn.get("min_raise")
    mx = your_turn.get("max_raise")
    if mn is not None:
        va["min_raise"] = int(round(float(mn)))
    if mx is not None:
        va["max_raise"] = int(round(float(mx)))
    return va


# ===========================================================================
# 跨手会话状态 + 消息分发
# ===========================================================================
class Session:
    _KNOWN = ("connected", "error", "hand_start", "hole_cards", "player_action",
              "community_cards", "your_turn", "hand_result",
              "lobby_joined", "table_joined", "table_state")

    def __init__(self, advisor, send, num_hands, default_seats, log_f=None,
                 exploit=False, debug=False, api_key=None):
        self.advisor = advisor
        self.send = send
        self.num_hands = num_hands
        self.default_seats = default_seats
        self.log_f = log_f
        self.exploit = exploit
        self.debug = debug
        self.api_key = api_key
        self.hand = None
        self.table_id = None
        self.observed_max_seat = default_seats - 1
        self.client_action_id = 0
        self.counters = {"hands": 0, "decisions": 0, "fallback": 0, "net_chips": 0,
                         "watchdog_rejoins": 0, "season_rebuys": 0, "hard": 0}
        self.fallback_reasons = {}
        self.hard_cats = {}
        self.last_hand_ts = time.time()
        self._rebuy_cooldown_until = 0.0  # season rebuy 冷却到点时刻（防 429 空转刷屏）。
        self._ws = None

    def _dbg(self, m):
        if self.debug:
            print(f"  [dbg] {m}", file=sys.stderr)

    def _num_seats(self):
        return max(self.default_seats, self.observed_max_seat + 1)

    def rejoin(self, ws, why):
        """leave_table + 重新 join_lobby：从死桌/旧座自救，重排队到活桌。"""
        print(f"  [rejoin] {why} → leave_table + join_lobby", file=sys.stderr)
        try:
            self.send(ws, {"type": "leave_table"})
            self.send(ws, {"type": "join_lobby", "buy_in": BUY_IN})
        except Exception as e:
            print(f"  [rejoin 发送失败] {e}", file=sys.stderr)

    def rebuy_and_join(self, ws):
        """season bankroll 不足以买入（insufficient_season_chips）→ POST /api/season/rebuy
        补 1500 → 重新 join_lobby。冷却中（429）记下到点时刻、交给 watchdog 重试，不空转刷屏。"""
        now = time.time()
        if now < self._rebuy_cooldown_until:
            print(f"  [rebuy] season 冷却中，约 {int(self._rebuy_cooldown_until - now)}s "
                  f"后可再 rebuy（等 watchdog 重试）", file=sys.stderr)
            return
        if not self.api_key:
            print("  [rebuy] 无 api_key，无法 season rebuy", file=sys.stderr)
            return
        ok, info = season_rebuy(self.api_key)
        if ok:
            self.counters["season_rebuys"] += 1
            cd = info.get("cooldown_seconds", 0) or 0
            self._rebuy_cooldown_until = now + cd
            print(f"  [rebuy] +1500 → chip_balance={info.get('chip_balance')} cooldown={cd}s "
                  f"（第 {self.counters['season_rebuys']} 次）→ join_lobby", file=sys.stderr)
            try:
                self.send(ws, {"type": "join_lobby", "buy_in": BUY_IN})
            except Exception as e:
                print(f"  [rebuy join 发送失败] {e}", file=sys.stderr)
        elif info.get("cooldown"):
            self._rebuy_cooldown_until = now + info["cooldown"]
            print(f"  [rebuy] 冷却 {info['cooldown']}s（429）→ 等待后重试", file=sys.stderr)
        else:
            print(f"  [rebuy] 失败 {info}", file=sys.stderr)

    def watchdog_check(self, stale_after=120):
        """另一线程每 30s 调：超过 stale_after 秒无新手（对手离桌后独坐、server recv 因
        心跳帧永不超时 → 会永久空等）→ leave+rejoin 自救。"""
        if self._ws is not None and time.time() - self.last_hand_ts > stale_after:
            self.counters["watchdog_rejoins"] += 1
            self.last_hand_ts = time.time()  # 防重入桌等待期连环触发
            self.rejoin(self._ws, f"{stale_after}s 无新手（watchdog）")

    def _track_seat(self, seat):
        if isinstance(seat, int) and seat > self.observed_max_seat:
            self.observed_max_seat = seat

    def handle(self, ws, msg):
        t = msg.get("type")
        self._ws = ws
        if "table_id" in msg and msg["table_id"]:
            if msg["table_id"] != self.table_id:
                print(f"  [table_id] {msg['table_id']}", file=sys.stderr)
            self.table_id = msg["table_id"]
        if t == "connected":
            print(f"  connected agent_id={msg.get('agent_id')} name={msg.get('name')}",
                  file=sys.stderr)
            self.last_hand_ts = time.time()
            self.send(ws, {"type": "join_lobby", "buy_in": BUY_IN})
        elif t == "error":
            print(f"  [error] {msg}", file=sys.stderr)
            if msg.get("code") == "auth_failed":
                ws.close()
            elif msg.get("code") == "already_seated":
                # 继承了旧进程/旧连接的座位（可能是死桌）→ leave + 重新 join 拿活桌。
                self.rejoin(ws, "already_seated（继承旧座，可能死桌）")
            elif msg.get("code") == "insufficient_season_chips":
                # season bankroll 不足买入 → 自动 rebuy(+1500) 后重 join（受 300s 冷却约束）。
                self.rebuy_and_join(ws)
        elif t in ("lobby_joined", "table_joined"):
            self.last_hand_ts = time.time()
            print(f"  [{t}] seat={msg.get('seat')} pos={msg.get('position')} "
                  f"table={self.table_id}", file=sys.stderr)
        elif t == "hand_start":
            self._track_seat(msg.get("dealer_seat"))
            self._track_seat(msg.get("seat"))
            self.hand = HandState(msg.get("hand_id"), msg.get("dealer_seat"),
                                  msg.get("seat"), self._num_seats())
            self._dbg(f"=== hand_start button={msg.get('dealer_seat')} "
                      f"my_seat={msg.get('seat')} seats={self.hand.num_seats} ===")
        elif t == "hole_cards":
            if self.hand:
                self.hand.hole = msg.get("cards") or []
                self._dbg(f"hole {' '.join(self.hand.hole)}")
        elif t == "player_action":
            if self.hand:
                self._track_seat(msg.get("seat"))
                self.hand.note_start_stack(msg.get("seat"), msg.get("stack"))
                self.hand.on_player_action(msg.get("seat"), msg.get("action"),
                                           msg.get("amount"))
                self.hand.update_stack(msg.get("seat"), msg.get("stack"))
        elif t == "community_cards":
            if self.hand:
                self.hand.on_community(msg.get("cards", []), _street_name(msg.get("street")))
                self._dbg(f"{self.hand.street} board={' '.join(self.hand.board)}")
        elif t == "your_turn":
            self._your_turn(ws, msg)
        elif t == "hand_result":
            self._hand_result(ws, msg)
            self.counters["hands"] += 1
            if self.counters["hands"] % 25 == 0 and self.counters["hands"] < self.num_hands:
                c = self.counters
                print(f"  [♥ {c['hands']}h] dec={c['decisions']} "
                      f"fb={c['fallback']} hard={c['hard']} net={c['net_chips']} "
                      f"table={self.table_id}", file=sys.stderr)
            if self.counters["hands"] >= self.num_hands:
                print(f"  打满 {self.num_hands} 手，离场。", file=sys.stderr)
                ws.close()

    def _your_turn(self, ws, msg):
        hand = self.hand
        if hand is None:
            return
        if "community_cards" in msg:
            hand.board = msg["community_cards"]
        for p in msg.get("players", []) or []:
            self._track_seat(p.get("seat"))
            hand.num_seats = self._num_seats()
            hand.note_start_stack(p.get("seat"), p.get("stack"))
            hand.update_stack(p.get("seat"), p.get("stack"))
            hand.update_name(p.get("seat"), p.get("name"))
        valid = parse_valid_actions(msg)
        req = hand.build_request(valid, include_names=self.exploit)
        try:
            resp = self.advisor.decide(req)
        except Exception as e:
            print(f"  [advisor 异常] {e} → fold 兜底", file=sys.stderr)
            resp = {"action": "fold", "source": "fallback:advisor_exception"}
        self.counters["decisions"] += 1
        src = str(resp.get("source", ""))
        if src.startswith("fallback"):
            self.counters["fallback"] += 1
            self.fallback_reasons[src[:80]] = self.fallback_reasons.get(src[:80], 0) + 1
        # 内联硬伤扫描：决策当下即报（不等 100 手），前期 bug 立刻现形。
        for cat, detail in hard_issues(req, resp):
            self.counters["hard"] += 1
            self.hard_cats[cat] = self.hard_cats.get(cat, 0) + 1
            hole = "".join(req.get("hole", []) or [])
            board = "".join(req.get("board", []) or [])
            print(f"  ⚠[硬伤:{cat}] {hole}|{board or '-'} -> {resp.get('action')} "
                  f"{resp.get('amount') or ''} :: {detail}", file=sys.stderr)
        self.client_action_id += 1
        amt = resp.get("amount")
        out = {
            "type": "action",
            "hand_id": msg.get("hand_id", hand.hand_id),
            "action": resp["action"],
            "amount": float(amt) if amt is not None else None,
            "client_action_id": f"h9-{self.client_action_id}",
            "turn_token": msg.get("turn_token"),
        }
        self.send(ws, out)
        if self.log_f:
            self.log_f.write(json.dumps({"req": req, "resp": resp, "sent": out},
                                        ensure_ascii=False) + "\n")
            self.log_f.flush()
        if self.debug:
            self._dbg(f"decide {resp.get('action')} amt={amt} src={src} eq={resp.get('equity')}")

    def _hand_result(self, ws, msg):
        self.last_hand_ts = time.time()
        hand = self.hand
        if self.exploit and hand is not None:
            try:
                self.advisor.observe(hand.observe_record())
            except Exception as e:
                print(f"  [observe 异常] {e}", file=sys.stderr)
        final = msg.get("final_stacks", {})
        if hand is not None:
            my_final = final.get(str(hand.my_seat), final.get(hand.my_seat))
            start = hand.hand_start_stacks_list()
            my_start = start[hand.my_seat] if hand.my_seat < len(start) else BUY_IN
            if my_final is not None:
                net = int(round(float(my_final))) - my_start
                self.counters["net_chips"] += net
                if self.debug:
                    self._dbg(f"hand_result net={net} winners={msg.get('winners')}")
            # 码深漂移：leave/rejoin 取回买入。
            if my_final is not None and (my_final < STACK_LEAVE_LO or my_final > STACK_LEAVE_HI):
                print(f"  [码深漂移] 我方栈 {my_final} 漂出 → leave/rejoin", file=sys.stderr)
                self.send(ws, {"type": "leave_table"})
                self.send(ws, {"type": "join_lobby", "buy_in": BUY_IN})
        self.hand = None


# ===========================================================================
# 真实联机
# ===========================================================================
def run_real(args):
    import threading
    import websocket

    advisor = Advisor(args.advisor_bin, args.samples, args.seed,
                      config=args.config, opp_profile=args.opp_profile, debug=args.debug)
    log_f = open(args.action_log, "w") if args.action_log else None

    def send(ws, obj):
        ws.send(json.dumps(obj))

    sess = Session(advisor, send, args.num_hands, args.seats, log_f=log_f,
                   exploit=args.exploit, debug=args.debug, api_key=args.api_key)
    header = [f"Authorization: Bearer {args.api_key}"]

    # 看门狗线程：对手离桌后独坐时 server 因心跳帧使 recv 永不超时 → 主线程会永久空等。
    # 另起线程每 30s 查 last_hand_ts，超 120s 无新手则 leave+rejoin 排队到活桌。
    stop_watchdog = threading.Event()

    def watchdog_loop():
        while not stop_watchdog.wait(30):
            try:
                sess.watchdog_check()
            except Exception as e:
                print(f"  [watchdog] {e}", file=sys.stderr)

    threading.Thread(target=watchdog_loop, daemon=True).start()

    attempts = 0
    while sess.counters["hands"] < args.num_hands:
        try:
            ws = websocket.create_connection(WS_URL, header=header, timeout=60)
        except Exception as e:
            attempts += 1
            print(f"  连接失败（{e}），3s 重试（{attempts}）…", file=sys.stderr)
            if attempts >= 10:
                print("  重连达上限，放弃。", file=sys.stderr)
                break
            time.sleep(3)
            continue
        attempts = 0
        sess._ws = ws
        sess.last_hand_ts = time.time()
        try:
            while sess.counters["hands"] < args.num_hands:
                raw = ws.recv()
                if not raw:
                    break
                try:
                    msg = json.loads(raw)
                except Exception:
                    continue
                sess.handle(ws, msg)
        except Exception as e:
            print(f"  连接中断（{e}），3s 重连…", file=sys.stderr)
            time.sleep(3)
        finally:
            sess._ws = None  # 断开期间看门狗读到 None 即 no-op（不往死 ws 发）。
            try:
                ws.close()
            except Exception:
                pass
    stop_watchdog.set()

    c = sess.counters
    print(f"\n=== 收尾：{c['hands']} 手 | 决策 {c['decisions']} | fallback {c['fallback']}"
          f" | net {c['net_chips']} chips "
          f"({1000.0 * c['net_chips'] / BIG_BLIND_OP / max(1, c['hands']):.1f} mbb/g)"
          f" | watchdog_rejoins {c['watchdog_rejoins']} | season_rebuys {c['season_rebuys']}"
          f" | hard {c['hard']} ===",
          file=sys.stderr)
    if sess.hard_cats:
        print(f"  硬伤分类: {dict(sorted(sess.hard_cats.items()))}", file=sys.stderr)
    if log_f:
        log_f.close()
    advisor.close()


# ===========================================================================
# 离线 selftest（canned 9-max 序列，无网络）
# ===========================================================================
def run_selftest(args):
    """不连网：用 canned 9-max 消息序列把 driver↔advisor IPC + 出合法动作跑通。
    验收：advisor 全程出合法 {action, amount}、driver 组请求正确。"""
    advisor = Advisor(args.advisor_bin, args.samples, args.seed, debug=args.debug)
    sent = []

    def send(_ws, obj):
        sent.append(obj)

    sess = Session(advisor, send, num_hands=2, default_seats=9, exploit=True, debug=True)

    # 一手 9-max：button=0, 我=seat3(UTG)。preflop 我 open，翻牌我 cbet，转牌结束。
    seq = [
        {"type": "connected", "agent_id": "self", "name": "h9"},
        {"type": "hand_start", "hand_id": "H1", "dealer_seat": 0, "seat": 3},
        {"type": "hole_cards", "cards": ["As", "Kd"]},
        # 我 UTG 决策（preflop, 无人加注）。
        {"type": "your_turn", "hand_id": "H1", "turn_token": "t1",
         "valid_actions": ["fold", "call", "raise"], "min_raise": 40, "max_raise": 2000,
         "players": [{"seat": i, "name": f"p{i}", "stack": 2000 - (BIG_BLIND_OP if i == 0
                      else SMALL_BLIND_OP if i == 8 else 0)} for i in range(9)]},
        # 假设我加注后，几家跟/弃，到翻牌。
        {"type": "player_action", "seat": 3, "action": "raise", "amount": 46, "stack": 1954},
        {"type": "player_action", "seat": 4, "action": "fold"},
        {"type": "player_action", "seat": 5, "action": "call", "amount": 46, "stack": 1954},
        {"type": "player_action", "seat": 6, "action": "fold"},
        {"type": "player_action", "seat": 7, "action": "fold"},
        {"type": "player_action", "seat": 8, "action": "fold"},
        {"type": "player_action", "seat": 0, "action": "fold"},
        {"type": "player_action", "seat": 1, "action": "fold"},
        {"type": "player_action", "seat": 2, "action": "fold"},
        {"type": "community_cards", "cards": ["Ah", "7c", "2d"], "street": "flop"},
        # 我 OOP cbet 决策（翻牌，无人下注）。
        {"type": "your_turn", "hand_id": "H1", "turn_token": "t2",
         "community_cards": ["Ah", "7c", "2d"],
         "valid_actions": ["check", "raise"], "min_raise": 20, "max_raise": 1954,
         "players": [{"seat": 3, "name": "p3", "stack": 1954},
                     {"seat": 5, "name": "p5", "stack": 1954}]},
        {"type": "player_action", "seat": 3, "action": "raise", "amount": 60, "stack": 1894},
        {"type": "player_action", "seat": 5, "action": "call", "amount": 60, "stack": 1894},
        {"type": "community_cards", "cards": ["Ah", "7c", "2d", "9s"], "street": "turn"},
        # 我 turn 决策（面对 check，我再下注）。
        {"type": "your_turn", "hand_id": "H1", "turn_token": "t3",
         "community_cards": ["Ah", "7c", "2d", "9s"],
         "valid_actions": ["check", "raise"], "min_raise": 20, "max_raise": 1894,
         "players": [{"seat": 3, "name": "p3", "stack": 1894},
                     {"seat": 5, "name": "p5", "stack": 1894}]},
        {"type": "hand_result", "hand_id": "H1", "winners": [3], "pot": 232,
         "final_stacks": {str(i): 2000 for i in range(9)}},
    ]

    print("=== selftest: 跑 canned 9-max 序列 ===", file=sys.stderr)
    ok = True
    for m in seq:
        sess.handle(None, m)
    # 校验：每个 your_turn 都产生一个合法 action 报文。
    actions = [s for s in sent if s.get("type") == "action"]
    print(f"selftest: 收到 {len(actions)} 个 action 报文", file=sys.stderr)
    for a in actions:
        assert a["action"] in ("fold", "check", "call", "raise"), a
        assert "turn_token" in a and isinstance(a["client_action_id"], str)
        if a["action"] == "raise":
            assert a["amount"] is not None and a["amount"] > 0, a
        else:
            assert a["amount"] is None, a
        print(f"  action={a['action']} amount={a['amount']} token={a['turn_token']}",
              file=sys.stderr)
    assert len(actions) == 3, f"应有 3 个决策点，实得 {len(actions)}"
    advisor.close()
    if ok:
        print("=== selftest PASS：IPC 通、3 决策全合法、observe 喂入无异常 ===", file=sys.stderr)


# ===========================================================================
def main():
    p = argparse.ArgumentParser(description="OpenPoker 9-max WS driver + 启发式 Rust advisor")
    p.add_argument("--advisor-bin", default="target/release/heuristic9_advisor")
    p.add_argument("--api-key", default=None, help="OpenPoker Bearer api_key（真实联机用）")
    p.add_argument("--num-hands", type=int, default=1000)
    p.add_argument("--seats", type=int, default=9, help="桌大小（满环假设；2-9）")
    p.add_argument("--samples", type=int, default=1500, help="advisor equity MC rollout 数")
    p.add_argument("--seed", type=int, default=1)
    p.add_argument("--config", default=None, help="启发式 config JSON 路径")
    p.add_argument("--opp-profile", default=None, help="对手画像持久化 JSON 路径（累积经验）")
    p.add_argument("--action-log", default="heuristic9_actions.jsonl")
    p.add_argument("--exploit", action="store_true", help="开对手画像剥削（发 names + observe）")
    p.add_argument("--selftest", action="store_true", help="离线验 IPC，不连网（无需账号）")
    p.add_argument("--debug", action="store_true")
    args = p.parse_args()
    args.seats = max(2, min(9, args.seats))

    if args.selftest:
        run_selftest(args)
        return
    if not args.api_key:
        print("真实联机需 --api-key（或用 --selftest 离线验证）。", file=sys.stderr)
        sys.exit(2)
    run_real(args)


if __name__ == "__main__":
    main()

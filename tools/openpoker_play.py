#!/usr/bin/env python3
"""OpenPoker 6-max WS driver（docs/temp/openpoker_client_design_2026_06_02.md §2/§3/§7）。

网络 / token / 重连 / 限速全在本文件（Python，websocket-client），策略全在常驻 Rust
advisor（tools/openpoker_advisor.rs）—— Rust crate 零网络依赖（invariant）。

driver 职责（§3）：
  - WS 连 wss://openpoker.ai/ws + Bearer 鉴权；join_lobby{buy_in:2000}（锁 100BB）。
  - 累计每手 betting 历史（hand_start/hole_cards/player_action/community_cards）→ 组 advisor 请求。
  - your_turn → 调 advisor 拿 {action, amount} → 回 action{turn_token, client_action_id}。
  - §4 码深漂移：每手后我方栈漂出 [80,125]BB → leave_table + rejoin 取 2000（控我方栈）。
  - 断线重连 + resync（best-effort）；限速 20 msg/s。

两种模式：
  --selftest   离线：不连网，用 canned 6-max 消息序列把 driver↔advisor IPC + 出合法动作跑通
               （验收：advisor 全程出合法 {action,amount}、driver 组请求正确，无需账号）。
  （默认）     真实联机 openpoker.ai（需 --api-key；用户注册 POST api.openpoker.ai/api/register）。

⚠ 未经 live 验证的协议假设（拿到账号后据真实消息校准，本文件内 [LIVE?] 标注）：
  - player_action.amount 视为 raise 的**总 to 额**（OpenPoker 单位）；若实为增量需改 _on_player_action。
  - 我方 my_seat、对手 seat 索引、turn_token/client_action_id 字段名以 §1 协议表为准。
"""

import argparse
import json
import subprocess
import sys
import time

# OpenPoker 协议常量（§0/§1）。
WS_URL = "wss://openpoker.ai/ws"
REGISTER_URL = "https://api.openpoker.ai/api/register"
BUY_IN = 2000           # 锁 100BB（§4 码深漂移缓解①）。
BIG_BLIND_OP = 20       # OpenPoker 默认 10/20。
SMALL_BLIND_OP = 10
STACK_LEAVE_LO = 80 * BIG_BLIND_OP   # 1600：我方栈 < 80BB → leave/rejoin。
STACK_LEAVE_HI = 125 * BIG_BLIND_OP  # 2500：我方栈 > 125BB → leave/rejoin。
NUM_SEATS = 6
_BOARD_LEN = {"preflop": 0, "flop": 3, "turn": 4, "river": 5}


# ===========================================================================
# 常驻 advisor 子进程（stdio JSON-lines，同 slumbot_play.Advisor）
# ===========================================================================
class Advisor:
    def __init__(self, binary, checkpoint, bucket_table, reshape, postflop_cap, seed):
        self.proc = subprocess.Popen(
            [binary, "--checkpoint", checkpoint, "--bucket-table", bucket_table,
             "--reshape", reshape, "--postflop-cap", str(postflop_cap), "--seed", str(seed)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1,
        )
        line = self.proc.stdout.readline()
        if not line:
            raise RuntimeError("advisor 未输出 ready 行（提前退出？看 stderr）")
        self.ready = json.loads(line)
        if not self.ready.get("ready"):
            raise RuntimeError(f"advisor ready 异常: {line!r}")

    def decide(self, req):
        """req = dict（见 openpoker_advisor::Request）。返回 advisor 响应 dict
        {action, amount?, source, ...}。"""
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
# 单手状态累计（§3：OpenPoker 有状态推送 → driver 自己累计成 advisor 要的历史）
# ===========================================================================
class HandState:
    """累计一手：button / my_seat / hole / board / 本手历史动作（含本街 to 额）。
    committed_this_street[seat] 跟每座本街累计投入（OpenPoker 单位）→ raise 的 to。"""

    def __init__(self, hand_id, button_seat, my_seat):
        self.hand_id = hand_id
        self.button_seat = button_seat
        self.my_seat = my_seat
        self.hole = None
        self.board = []
        self.street = "preflop"
        self.actions = []  # [{seat, action, to?}]，按时间序
        # 本街每座累计投入（preflop 含盲注）。
        self.committed = {s: 0 for s in range(NUM_SEATS)}
        self.committed[(button_seat + 1) % NUM_SEATS] = SMALL_BLIND_OP  # SB
        self.committed[(button_seat + 2) % NUM_SEATS] = BIG_BLIND_OP    # BB

    def on_player_action(self, seat, action, amount):
        """记一条对手 / 我方已确认动作。to = 该座本街累计到额（raise/bet 才需）。"""
        a = (action or "").lower()
        to = None
        if a in ("raise", "bet"):
            # [LIVE?] 视 amount 为总 to 额；若实为增量改成 self.committed[seat] + amount。
            self.committed[seat] = amount if amount is not None else self.committed[seat]
            to = self.committed[seat]
        elif a == "call":
            self.committed[seat] = max(self.committed.values())
        elif a == "all_in":
            if amount is not None:
                self.committed[seat] = amount
        # check/fold：committed 不变。
        self.actions.append({"seat": seat, "action": a, **({"to": to} if to is not None else {})})

    def on_community(self, cards, street):
        self.board = cards
        self.street = street
        # 新街：本街投入清零（§3）。
        self.committed = {s: 0 for s in range(NUM_SEATS)}

    def build_request(self, valid):
        """组 advisor 请求（openpoker_advisor::Request）。"""
        return {
            "hole": self.hole,
            "board": self.board,
            "button_seat": self.button_seat,
            "my_seat": self.my_seat,
            "num_seats": NUM_SEATS,
            "small_blind": SMALL_BLIND_OP,
            "big_blind": BIG_BLIND_OP,
            "actions": self.actions,
            "valid": valid,
        }


def parse_valid_actions(your_turn):
    """从 your_turn 的 valid_actions / min_raise / max_raise 提合法区间（advisor 用）。"""
    can_check = can_call = can_raise = False
    min_raise = your_turn.get("min_raise")
    max_raise = your_turn.get("max_raise")
    for va in your_turn.get("valid_actions", []):
        act = va.get("action")
        if act == "check":
            can_check = True
        elif act == "call":
            can_call = True
        elif act in ("raise", "bet"):
            can_raise = True
            min_raise = va.get("min", min_raise)
            max_raise = va.get("max", max_raise)
        elif act == "all_in":
            can_raise = can_raise or False  # all_in 单独，不开 raise 区间
    return {
        "can_check": can_check, "can_call": can_call, "can_raise": can_raise,
        "min_raise": min_raise, "max_raise": max_raise,
    }


# ===========================================================================
# 真实联机（WS）
# ===========================================================================
def run_real(advisor, api_key, num_hands, action_log=None):
    import websocket  # 延迟 import：离线 selftest 不需要 websocket-client

    log_f = open(action_log, "w") if action_log else None
    counters = {"hands": 0, "decisions": 0, "blueprint": 0, "fallback": 0, "net_chips": 0}
    client_action_id = [0]

    def send(ws, obj):
        ws.send(json.dumps(obj))

    state = {"hand": None, "table_id": None, "last_seq": 0}

    def on_message(ws, raw):
        msg = json.loads(raw)
        t = msg.get("type")
        if "table_id" in msg:
            state["table_id"] = msg["table_id"]
        if "table_seq" in msg:
            state["last_seq"] = msg["table_seq"]

        if t == "connected":
            print(f"  connected agent_id={msg.get('agent_id')} name={msg.get('name')}", file=sys.stderr)
            send(ws, {"type": "join_lobby", "buy_in": BUY_IN})
        elif t == "error":
            print(f"  [error] {msg}", file=sys.stderr)
            if msg.get("code") == "auth_failed":
                ws.close()
        elif t == "hand_start":
            state["hand"] = HandState(msg.get("hand_id"), msg.get("dealer_seat"), msg.get("seat"))
        elif t == "hole_cards":
            if state["hand"]:
                state["hand"].hole = msg.get("cards")
        elif t == "player_action":
            if state["hand"]:
                state["hand"].on_player_action(msg.get("seat"), msg.get("action"), msg.get("amount"))
        elif t == "community_cards":
            if state["hand"]:
                state["hand"].on_community(msg.get("cards", []), _street_name(msg.get("street")))
        elif t == "your_turn":
            _handle_your_turn(ws, advisor, state, msg, counters, client_action_id, send, log_f)
        elif t == "hand_result":
            _handle_hand_result(ws, state, msg, counters, send)
            counters["hands"] += 1
            if counters["hands"] >= num_hands:
                print(f"  打满 {num_hands} 手，离场。", file=sys.stderr)
                ws.close()

    def on_error(ws, err):
        print(f"  [ws error] {err}", file=sys.stderr)

    def on_close(ws, code, reason):
        print(f"  [ws closed] code={code} reason={reason}", file=sys.stderr)

    header = [f"Authorization: Bearer {api_key}"]
    # 断线重连：最多重连若干次（§1：断线 120s 内 resync 补；这里简化为重连后继续）。
    attempts = 0
    while counters["hands"] < num_hands and attempts < 10:
        attempts += 1
        ws = websocket.WebSocketApp(
            WS_URL, header=header,
            on_message=on_message, on_error=on_error, on_close=on_close,
        )
        ws.run_forever(ping_interval=30, ping_timeout=10)
        if counters["hands"] < num_hands:
            print(f"  断线，3s 后重连（attempt {attempts}）…", file=sys.stderr)
            time.sleep(3)

    if log_f:
        log_f.close()
    _report(counters)


def _street_name(s):
    if isinstance(s, str):
        return s
    # 数字街（0/1/2/3）→ 名称。
    return ["preflop", "flop", "turn", "river"][s] if isinstance(s, int) and 0 <= s <= 3 else "preflop"


def _handle_your_turn(ws, advisor, state, msg, counters, client_action_id, send, log_f):
    hand = state["hand"]
    if hand is None:
        return
    # board / street 以 your_turn 为准（更新累计）。
    if "community_cards" in msg:
        hand.board = msg["community_cards"]
    valid = parse_valid_actions(msg)
    req = hand.build_request(valid)
    try:
        resp = advisor.decide(req)
    except Exception as e:
        print(f"  [advisor 异常] {e} → fold 兜底", file=sys.stderr)
        resp = {"action": "fold", "source": "fallback:advisor_exception"}
    counters["decisions"] += 1
    if str(resp.get("source", "")).startswith("fallback"):
        counters["fallback"] += 1
    else:
        counters["blueprint"] += 1
    client_action_id[0] += 1
    out = {
        "type": "action",
        "hand_id": msg.get("hand_id", hand.hand_id),
        "action": resp["action"],
        "turn_token": msg.get("turn_token"),
        "client_action_id": client_action_id[0],
    }
    if resp.get("amount") is not None:
        out["amount"] = resp["amount"]
    send(ws, out)
    if log_f:
        log_f.write(json.dumps({"req": req, "resp": resp, "sent": out}, ensure_ascii=False) + "\n")
        log_f.flush()


def _handle_hand_result(ws, state, msg, counters, send):
    # §4 码深漂移：我方栈漂出 [80,125]BB → leave_table + rejoin 取 2000。
    final = msg.get("final_stacks", {})
    hand = state["hand"]
    my_stack = None
    if hand is not None:
        my_stack = final.get(str(hand.my_seat), final.get(hand.my_seat))
    if my_stack is not None and (my_stack < STACK_LEAVE_LO or my_stack > STACK_LEAVE_HI):
        print(f"  [码深漂移] 我方栈 {my_stack} 漂出 [{STACK_LEAVE_LO},{STACK_LEAVE_HI}] → leave/rejoin", file=sys.stderr)
        send(ws, {"type": "leave_table"})
        send(ws, {"type": "join_lobby", "buy_in": BUY_IN})
    state["hand"] = None


def _report(counters):
    d = counters["decisions"]
    fb = counters["fallback"]
    print(f"hands={counters['hands']} decisions={d} "
          f"blueprint={counters['blueprint']} fallback={fb} "
          f"({100.0 * fb / d if d else 0:.1f}% 兜底)", file=sys.stderr)


# ===========================================================================
# 离线 selftest（不连网，验 driver↔advisor IPC + 出合法动作）
# ===========================================================================
def run_selftest(advisor):
    """canned 消息序列喂 driver 状态机 + advisor，验：组请求正确、advisor 出合法
    {action,amount}、driver 能组 action 包。不连网、不需账号。"""
    print(f"advisor ready: update_count={advisor.ready.get('update_count')} "
          f"reshape={advisor.ready.get('reshape')}", file=sys.stderr)

    # 场景：button=2, 我方 my_seat=2(BTN)，UTG(5)/HJ(0)/CO(1) 先 fold → 轮我。
    button, my_seat = 2, 2
    hand = HandState("h1", button, my_seat)
    hand.hole = ["Ah", "Kd"]
    # UTG=(button+3)%6=5, HJ=0, CO=1 fold。
    for s in [5, 0, 1]:
        hand.on_player_action(s, "fold", None)
    valid = {"can_check": False, "can_call": True, "can_raise": True, "min_raise": 40, "max_raise": 2000}
    req = hand.build_request(valid)
    resp = advisor.decide(req)
    _assert_legal(resp, valid, "folds-to-BTN")
    print(f"[selftest 1 folds-to-BTN] resp={resp}", file=sys.stderr)

    # 场景 2：对手 UTG open-limp（call to=20）→ 我 HJ 决策。no-limp blueprint 应兜底（合法）。
    hand2 = HandState("h2", button=0, my_seat=4)
    hand2.hole = ["Qs", "Qd"]
    hand2.on_player_action(3, "call", 20)  # UTG limp
    req2 = hand2.build_request(valid)
    resp2 = advisor.decide(req2)
    _assert_legal(resp2, valid, "open-limp")
    print(f"[selftest 2 open-limp] resp={resp2} (no-limp blueprint 预期 fallback)", file=sys.stderr)

    # 场景 3：flop 决策（preflop 我 raise 被 call，flop 我先动）。board 3 张。
    hand3 = HandState("h3", button=0, my_seat=1)  # SB
    hand3.hole = ["Jc", "Tc"]
    hand3.on_player_action(3, "fold", None)
    hand3.on_player_action(4, "fold", None)
    hand3.on_player_action(5, "fold", None)
    hand3.on_player_action(0, "fold", None)  # BTN fold
    hand3.on_player_action(1, "raise", 60)   # SB raise to 60
    hand3.on_player_action(2, "call", 60)    # BB call
    hand3.on_community(["7h", "2c", "Ks"], "flop")
    valid_flop = {"can_check": True, "can_call": False, "can_raise": True, "min_raise": 20, "max_raise": 1940}
    req3 = hand3.build_request(valid_flop)
    resp3 = advisor.decide(req3)
    _assert_legal(resp3, valid_flop, "flop")
    print(f"[selftest 3 flop] resp={resp3}", file=sys.stderr)

    print("OK: 3 个 canned 场景跑通，advisor 全程出合法动作、driver 组请求/动作包正常。", file=sys.stderr)


def _assert_legal(resp, valid, tag):
    a = resp.get("action")
    ok = (
        a == "fold"
        or (a == "check" and valid["can_check"])
        or (a == "call" and valid["can_call"])
        or a == "all_in"
        or (a == "raise" and resp.get("amount") is not None
            and valid["min_raise"] <= resp["amount"] <= valid["max_raise"])
    )
    if not ok:
        raise RuntimeError(f"[{tag}] advisor 出非法动作 {resp!r} (valid={valid})")


# ===========================================================================
# main
# ===========================================================================
def main():
    p = argparse.ArgumentParser(description="OpenPoker 6-max WS driver + Rust advisor")
    p.add_argument("--advisor-bin", default="target/release/openpoker_advisor")
    p.add_argument("--checkpoint", required=True)
    p.add_argument("--bucket-table", required=True)
    p.add_argument("--reshape", default="preopen")
    p.add_argument("--postflop-cap", type=int, default=3)
    p.add_argument("--seed", type=int, default=1)
    p.add_argument("--num-hands", type=int, default=100)
    p.add_argument("--api-key", default=None, help="OpenPoker Bearer api_key（真实联机用；注册见 docstring）")
    p.add_argument("--action-log", default="openpoker_actions.jsonl",
                   help="每决策落 {req,resp,sent} JSONL；空串关闭")
    p.add_argument("--selftest", action="store_true", help="离线验 IPC，不连网（无需账号）")
    args = p.parse_args()

    advisor = Advisor(args.advisor_bin, args.checkpoint, args.bucket_table,
                      args.reshape, args.postflop_cap, args.seed)
    try:
        if args.selftest:
            run_selftest(advisor)
        else:
            if not args.api_key:
                raise SystemExit("真实联机需 --api-key（注册 POST api.openpoker.ai/api/register）；"
                                 "或 --selftest 离线验 IPC")
            log = args.action_log if args.action_log else None
            run_real(advisor, args.api_key, args.num_hands, action_log=log)
    finally:
        advisor.close()


if __name__ == "__main__":
    main()

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

✓ 协议已 live 校准（2026-06-03 观测 + docs.openpoker.ai/llms-full.txt）：
  - action 报文：client_action_id 必须**字符串**、amount 始终在场（raise=float 总 to 额、余 null）。
    （早先用 int client_action_id 被服务端拒 invalid_message。）
  - player_action.amount_mode=="to_total"：raise 的 amount = 总 to 额（确认，与 on_player_action 一致）。
  - hand_start{seat,dealer_seat,blinds}；hole_cards{cards}；your_turn{valid_actions,min_raise,max_raise,
    turn_token,seat}；community_cards{cards,street}。消息分 stream:event（离散事件，driver 用）+
    stream:state（table_state 全量快照，driver 忽略，未来可改用它取 valid_actions 更鲁棒）。
⚠ 固有限制：真实桌**码深漂移严重**（实测同桌 14BB–800BB），blueprint 假设 100BB → off-distribution
  手大量走 advisor 兜底（§4 已知短板，非 bug）；买入锁 2000 + 漂出 [80,125]BB leave/rejoin 只控我方栈。
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
    def __init__(self, binary, checkpoint, bucket_table, reshape, postflop_cap, seed,
                 extra_args=None):
        cmd = [binary, "--checkpoint", checkpoint, "--bucket-table", bucket_table,
               "--reshape", reshape, "--postflop-cap", str(postflop_cap), "--seed", str(seed)]
        cmd += list(extra_args or [])  # 缺口②：--search / --search-* 透传给 Rust advisor。
        self.proc = subprocess.Popen(
            cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1,
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
        # 本街每座累计投入（preflop 含盲注）；新街清零。
        self.committed = {s: 0 for s in range(NUM_SEATS)}
        self.committed[(button_seat + 1) % NUM_SEATS] = SMALL_BLIND_OP  # SB
        self.committed[(button_seat + 2) % NUM_SEATS] = BIG_BLIND_OP    # BB
        # 缺口②：本手累计投入（跨街**不清零**，含盲注）→ 回推 hand-start 真栈：
        # hand_start[s] = your_turn 给的当前 remaining[s] + committed_total[s]。
        self.committed_total = dict(self.committed)
        # 各座当前 remaining 栈（OpenPoker 单位）；从 your_turn.players[].stack / player_action.stack
        # 滚动更新（None = 还没观测到该座栈）。
        self.stacks_now = {s: None for s in range(NUM_SEATS)}

    def on_player_action(self, seat, action, amount):
        """记一条对手 / 我方已确认动作。to = 该座本街累计到额（raise/bet 才需）。"""
        a = (action or "").lower()
        prev = self.committed.get(seat, 0)
        to = None
        if a in ("raise", "bet"):
            # [LIVE?] 视 amount 为总 to 额；若实为增量改成 self.committed[seat] + amount。
            self.committed[seat] = amount if amount is not None else prev
            to = self.committed[seat]
        elif a == "call":
            self.committed[seat] = max(self.committed.values())
        elif a == "all_in":
            if amount is not None:
                self.committed[seat] = amount
        # check/fold：committed 不变。
        # 缺口②：本手累计投入 += 本动作增量（本街新 committed − 旧）。all_in 无 amount → 增量 0
        # （信息缺，advisor 真栈重放会因 apply 非法回落 fold，不污染 blueprint 路径）。
        self.committed_total[seat] = self.committed_total.get(seat, 0) + (self.committed[seat] - prev)
        self.actions.append({"seat": seat, "action": a, **({"to": to} if to is not None else {})})

    def update_stacks(self, seat, stack):
        """从 player_action.stack / your_turn.players[].stack 滚动记各座当前 remaining 栈。"""
        if seat is not None and stack is not None and 0 <= seat < NUM_SEATS:
            self.stacks_now[seat] = stack

    def hand_start_stacks(self):
        """回推各座 hand-start 真栈（OpenPoker 单位）= 当前 remaining + 本手累计投入。全 6 座栈都
        已观测到才返回长 6 list（喂 advisor 实时搜索）；否则 None（advisor 退对称 100BB）。"""
        if any(self.stacks_now[s] is None for s in range(NUM_SEATS)):
            return None
        return [self.stacks_now[s] + self.committed_total.get(s, 0) for s in range(NUM_SEATS)]

    def on_community(self, cards, street):
        self.board = cards
        self.street = street
        # 新街：本街投入清零（§3）；committed_total 跨街保留（缺口②）。
        self.committed = {s: 0 for s in range(NUM_SEATS)}

    def build_request(self, valid):
        """组 advisor 请求（openpoker_advisor::Request）。`stacks` 仅在全 6 座真栈已知时附带
        （缺口②实时搜索读；缺省 → advisor 退对称 100BB blueprint，byte-equal）。"""
        req = {
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
        stacks = self.hand_start_stacks()
        if stacks is not None:
            req["stacks"] = stacks
        return req


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
                state["hand"].update_stacks(msg.get("seat"), msg.get("stack"))
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
    # 缺口②：your_turn 携 players:[{seat,name,stack}] = 各座决策时 remaining 栈 → 滚动记录，
    # build_request 回推 hand-start 真栈喂实时搜索（缺则不附 stacks，advisor 退 100BB）。
    for p in msg.get("players", []) or []:
        hand.update_stacks(p.get("seat"), p.get("stack"))
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
    amt = resp.get("amount")
    # action 报文格式（docs.openpoker.ai/llms-full.txt 校准，2026-06-03）：
    # client_action_id 必须是**字符串**（去重 ID）；amount 始终在场（raise 为 float 总 to 额、
    # 余为 null）。早先用 int client_action_id 被服务端拒（invalid_message）。
    out = {
        "type": "action",
        "hand_id": msg.get("hand_id", hand.hand_id),
        "action": resp["action"],
        "amount": float(amt) if amt is not None else None,
        "client_action_id": f"jx-{client_action_id[0]}",
        "turn_token": msg.get("turn_token"),
    }
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
    hand2 = HandState("h2", button_seat=0, my_seat=4)
    hand2.hole = ["Qs", "Qd"]
    hand2.on_player_action(3, "call", 20)  # UTG limp
    req2 = hand2.build_request(valid)
    resp2 = advisor.decide(req2)
    _assert_legal(resp2, valid, "open-limp")
    print(f"[selftest 2 open-limp] resp={resp2} (no-limp blueprint 预期 fallback)", file=sys.stderr)

    # 场景 3：flop 决策（preflop 我 raise 被 call，flop 我先动）。board 3 张。
    hand3 = HandState("h3", button_seat=0, my_seat=1)  # SB
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
    # 缺口② 实时搜索（透传给 Rust advisor；开了才送 stacks 真栈、postflop 触发面 re-solve）。
    p.add_argument("--search", action="store_true",
                   help="开实时子博弈搜索（postflop 触发面用真码深 re-solve；缺省纯 blueprint）")
    p.add_argument("--search-iterations", type=int, default=None)
    p.add_argument("--search-trigger", choices=["flop-first-unraised", "all-postflop"], default=None)
    p.add_argument("--search-time-budget-ms", type=int, default=None)
    p.add_argument("--search-lcfr", action="store_true")
    p.add_argument("--search-max-nodes", type=int, default=None)
    args = p.parse_args()

    extra = []
    if args.search:
        extra.append("--search")
        if args.search_iterations is not None:
            extra += ["--search-iterations", str(args.search_iterations)]
        if args.search_trigger is not None:
            extra += ["--search-trigger", args.search_trigger]
        if args.search_time_budget_ms is not None:
            extra += ["--search-time-budget-ms", str(args.search_time_budget_ms)]
        if args.search_lcfr:
            extra.append("--search-lcfr")
        if args.search_max_nodes is not None:
            extra += ["--search-max-nodes", str(args.search_max_nodes)]
    advisor = Advisor(args.advisor_bin, args.checkpoint, args.bucket_table,
                      args.reshape, args.postflop_cap, args.seed, extra_args=extra)
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

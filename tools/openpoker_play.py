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

HH 日志（--hh-log，realtime_search_openpoker_exec §4.2 数据管道）：每手 hand_result 落一行
**全桌手牌历史** JSONL——整手动作序 + board + 对手 name + 回推 hand-start 真栈 +
winners/final_stacks/shown_cards 原样，喂 Rust 侧 `openpoker_hh_aivat`
（openpoker_hh::hh_to_multiway_input → 多人 AIVAT，mbb/g）。**advisor 路径隔离**：HH 只读
已累计状态、绝不动 build_request 消费的字段；挂/不挂 --hh-log advisor 请求/输出 byte-equal
由 --selftest 的 canned 序列钉死（场景 5）。

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

# HH 日志（§4.2）：player_action 原始字段子集，平行落进 actions_ext（advisor 不读）。
# contribution_delta / stack_before / stack_after 是 live 校准发现的字段——比 driver 自跟
# committed 更稳（all_in 无 amount 时 committed_total 有已知缺口），Rust 解析侧留作修复材料。
_EXT_KEYS = ("amount", "amount_mode", "street", "stack", "stack_before", "stack_after",
             "contribution_delta", "to_call_before")


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
        # HH 日志（§4.2）：与 actions 平行的 player_action 原始字段子集 + 本手观测的
        # seat→name。advisor 的 build_request 不读这两个（byte-equal 隔离，selftest 5 钉死）。
        self.actions_ext = []
        self.names = {}
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

    def on_player_action(self, seat, action, amount, ext=None):
        """记一条对手 / 我方已确认动作。to = 该座本街累计到额（raise/bet 才需）。
        ext = player_action 原始字段子集（HH 日志用，advisor 不读）。"""
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
        self.actions_ext.append(dict(ext) if ext else {})

    def update_stacks(self, seat, stack):
        """从 player_action.stack / your_turn.players[].stack 滚动记各座当前 remaining 栈。
        int(round(.)) 归一：服务端 JSON 可能给 float（2000.0），advisor Request.stacks 是 u64。"""
        if seat is not None and stack is not None and 0 <= seat < NUM_SEATS:
            self.stacks_now[seat] = int(round(stack))

    def update_name(self, seat, name):
        """从 your_turn.players[].name 记 seat→name（HH 日志：对手可追踪性，§4.2 目的 2）。"""
        if seat is not None and name is not None and 0 <= seat < NUM_SEATS:
            self.names[seat] = name

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

    def hh_record(self, hand_result_msg):
        """一手 HH JSONL 记录（§4.2 数据管道）。hand_result 的 winners/final_stacks/
        shown_cards/pot **原样保留**（不做有损映射，单位换算在 Rust 解析侧）；只读状态不写。"""
        return {
            "hh": 1,
            "ts": round(time.time(), 3),
            "hand_id": self.hand_id,
            "button_seat": self.button_seat,
            "my_seat": self.my_seat,
            "num_seats": NUM_SEATS,
            "small_blind": SMALL_BLIND_OP,
            "big_blind": BIG_BLIND_OP,
            "hole": self.hole,
            "board": self.board,
            "street": self.street,
            "actions": self.actions,
            "actions_ext": self.actions_ext,
            "names": self.names,
            "stacks_start": self.hand_start_stacks(),
            "committed_total": self.committed_total,
            "hand_result": {k: hand_result_msg.get(k)
                            for k in ("winners", "final_stacks", "shown_cards", "pot")},
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
# 消息路由（真实联机与 selftest canned 序列共用同一条路径——HH byte-equal 隔离
# 测试靠这份共用才测到真实代码，不是平行复刻）
# ===========================================================================
class Session:
    """跨手状态 + 消息分发。`send(ws, obj)` 由调用方注入（真实 = ws.send；canned = 收集）。"""

    # handle_message 显式处理的消息类型；其余类型首见时打 stderr（看清被踢 / 桌散 /
    # 移桌这类**沉默事件**的真实报文——live 实测 2026-06-11 曾被移出桌后空等）。
    _KNOWN_TYPES = ("connected", "error", "hand_start", "hole_cards", "player_action",
                    "community_cards", "your_turn", "hand_result",
                    "lobby_joined", "table_joined")

    def __init__(self, advisor, send, num_hands, log_f=None, hh_f=None):
        self.advisor = advisor
        self.send = send
        self.num_hands = num_hands
        self.log_f = log_f
        self.hh_f = hh_f
        self.counters = {"hands": 0, "decisions": 0, "blueprint": 0, "search": 0,
                         "fallback": 0, "net_chips": 0, "hh_hands": 0, "hh_skipped": 0,
                         "watchdog_rejoins": 0}
        self.state = {"hand": None, "table_id": None, "last_seq": 0}
        self.client_action_id = [0]
        # 看门狗（run_real 的后台线程读写）：最近一次 hand_result / 任意消息的时刻 + 当前 ws。
        self.last_hand_ts = time.time()
        self._ws = None
        self._seen_types = set()

    def rejoin(self, ws, why):
        """leave + 重新 join_lobby 拿干净座位（已不在桌 / 卡死自救；服务端对未坐桌的
        leave_table 容忍）。"""
        print(f"  [rejoin] {why} → leave_table + join_lobby", file=sys.stderr)
        self.send(ws, {"type": "leave_table"})
        self.send(ws, {"type": "join_lobby", "buy_in": BUY_IN})

    def watchdog_check(self, stale_after=300):
        """run_real 后台线程每 30s 调一次：超过 stale_after 秒没有 hand_result（正常节奏
        ~1 手/分钟）≈ 被移出桌 / 桌散后空等（live 实测 2026-06-11：driver 只在 connected
        时 join 一次，被移出后会永远空等）→ leave+rejoin 自救。"""
        if self._ws is not None and time.time() - self.last_hand_ts > stale_after:
            self.counters["watchdog_rejoins"] += 1
            self.last_hand_ts = time.time()  # 防重入桌等待期间连环触发
            try:
                self.rejoin(self._ws, f"{stale_after}s 无 hand_result（watchdog）")
            except Exception as e:
                print(f"  [watchdog] rejoin 发送失败: {e}", file=sys.stderr)

    def handle_message(self, ws, msg):
        t = msg.get("type")
        self._ws = ws
        if "table_id" in msg:
            # 换桌（或首次入桌）时打观战链接（arena 页面按 table_id 路由）。
            if msg["table_id"] and msg["table_id"] != self.state["table_id"]:
                print(f"  [table] table_id={msg['table_id']} "
                      f"观战: https://openpoker.ai/zh/arena/{msg['table_id']}", file=sys.stderr)
            self.state["table_id"] = msg["table_id"]
        if "table_seq" in msg:
            self.state["last_seq"] = msg["table_seq"]
        if t not in self._seen_types:
            self._seen_types.add(t)
            if t not in self._KNOWN_TYPES:
                print(f"  [msg] 未处理消息类型 {t!r} keys={sorted(msg.keys())}", file=sys.stderr)

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
                # 上个进程的座位被服务端保留（live 实测）：可能在死桌上——主动换干净座位。
                self.rejoin(ws, "already_seated（继承了旧进程的座位，可能是死桌）")
        elif t in ("lobby_joined", "table_joined"):
            # 排队 / 入座流程中没有 hand_result 是正常状态——喂狗，免得看门狗在大厅队列里
            # 误触发 rejoin（会被重排队尾、可能循环）。
            self.last_hand_ts = time.time()
            print(f"  [{t}] position={msg.get('position')} wait={msg.get('estimated_wait')} "
                  f"seat={msg.get('seat')}", file=sys.stderr)
        elif t == "hand_start":
            self.state["hand"] = HandState(msg.get("hand_id"), msg.get("dealer_seat"),
                                           msg.get("seat"))
        elif t == "hole_cards":
            if self.state["hand"]:
                self.state["hand"].hole = msg.get("cards")
        elif t == "player_action":
            if self.state["hand"]:
                ext = {k: msg[k] for k in _EXT_KEYS if k in msg}
                self.state["hand"].on_player_action(msg.get("seat"), msg.get("action"),
                                                    msg.get("amount"), ext=ext)
                self.state["hand"].update_stacks(msg.get("seat"), msg.get("stack"))
        elif t == "community_cards":
            if self.state["hand"]:
                self.state["hand"].on_community(msg.get("cards", []),
                                                _street_name(msg.get("street")))
        elif t == "your_turn":
            self._handle_your_turn(ws, msg)
        elif t == "hand_result":
            self._handle_hand_result(ws, msg)
            self.counters["hands"] += 1
            if self.counters["hands"] >= self.num_hands:
                print(f"  打满 {self.num_hands} 手，离场。", file=sys.stderr)
                ws.close()

    def _handle_your_turn(self, ws, msg):
        hand = self.state["hand"]
        if hand is None:
            return
        # board / street 以 your_turn 为准（更新累计）。
        if "community_cards" in msg:
            hand.board = msg["community_cards"]
        # 缺口②：your_turn 携 players:[{seat,name,stack}] = 各座决策时 remaining 栈 → 滚动记录，
        # build_request 回推 hand-start 真栈喂实时搜索（缺则不附 stacks，advisor 退 100BB）。
        # name 同步进 HH（advisor 不读）。
        for p in msg.get("players", []) or []:
            hand.update_stacks(p.get("seat"), p.get("stack"))
            hand.update_name(p.get("seat"), p.get("name"))
        valid = parse_valid_actions(msg)
        req = hand.build_request(valid)
        try:
            resp = self.advisor.decide(req)
        except Exception as e:
            print(f"  [advisor 异常] {e} → fold 兜底", file=sys.stderr)
            resp = {"action": "fold", "source": "fallback:advisor_exception"}
        self.counters["decisions"] += 1
        # source 分桶（缺口②）：blueprint=blueprint 策略；search=实时搜索解出（含脱影子
        # search:unanchored，缺口②续）；fallback=兜底（blueprint 结构性 fallback:* + 搜索解不出来
        # search_giveup:* 都算「兜底」§4.1 护栏）。注意顺序：search_giveup 也以 "search" 开头，先判兜底。
        src = str(resp.get("source", ""))
        if src.startswith("fallback") or src.startswith("search_giveup"):
            self.counters["fallback"] += 1
        elif src.startswith("search"):
            self.counters["search"] += 1
        else:
            self.counters["blueprint"] += 1
        self.client_action_id[0] += 1
        amt = resp.get("amount")
        # action 报文格式（docs.openpoker.ai/llms-full.txt 校准，2026-06-03）：
        # client_action_id 必须是**字符串**（去重 ID）；amount 始终在场（raise 为 float 总 to 额、
        # 余为 null）。早先用 int client_action_id 被服务端拒（invalid_message）。
        out = {
            "type": "action",
            "hand_id": msg.get("hand_id", hand.hand_id),
            "action": resp["action"],
            "amount": float(amt) if amt is not None else None,
            "client_action_id": f"jx-{self.client_action_id[0]}",
            "turn_token": msg.get("turn_token"),
        }
        self.send(ws, out)
        if self.log_f:
            self.log_f.write(json.dumps({"req": req, "resp": resp, "sent": out},
                                        ensure_ascii=False) + "\n")
            self.log_f.flush()

    def _handle_hand_result(self, ws, msg):
        self.last_hand_ts = time.time()
        hand = self.state["hand"]
        # HH 日志（§4.2）：整手落一行（在清空 hand 之前）。只读已累计状态 + hand_result 原样，
        # 不动 advisor 消费的任何字段（byte-equal 由 selftest 5 钉死）。
        if self.hh_f is not None:
            if hand is not None:
                rec = hand.hh_record(msg)
                # table_id 是会话态非单手态 → 在落盘点补（按桌分析对手池用；Rust 解析侧忽略）。
                rec["table_id"] = self.state["table_id"]
                self.hh_f.write(json.dumps(rec, ensure_ascii=False) + "\n")
                self.hh_f.flush()
                self.counters["hh_hands"] += 1
            else:
                # 中途入桌 / 重连后第一手：本手没跟到开头，丢弃并计数（不落半截手）。
                self.counters["hh_skipped"] += 1
        # §4 码深漂移：我方栈漂出 [80,125]BB → leave_table + rejoin 取 2000。
        final = msg.get("final_stacks", {})
        my_stack = None
        if hand is not None:
            my_stack = final.get(str(hand.my_seat), final.get(hand.my_seat))
        if my_stack is not None and (my_stack < STACK_LEAVE_LO or my_stack > STACK_LEAVE_HI):
            print(f"  [码深漂移] 我方栈 {my_stack} 漂出 [{STACK_LEAVE_LO},{STACK_LEAVE_HI}]"
                  f" → leave/rejoin", file=sys.stderr)
            self.send(ws, {"type": "leave_table"})
            self.send(ws, {"type": "join_lobby", "buy_in": BUY_IN})
        self.state["hand"] = None


# ===========================================================================
# 真实联机（WS）
# ===========================================================================
def _ws_send(ws, obj):
    ws.send(json.dumps(obj))


def run_real(advisor, api_key, num_hands, action_log=None, hh_log=None):
    import threading

    import websocket  # 延迟 import：离线 selftest 不需要 websocket-client

    log_f = open(action_log, "w") if action_log else None
    # HH 用 append：贯穿全程的后台采集（§4.2），跨重连 / 多次运行累积同一个文件。
    hh_f = open(hh_log, "a") if hh_log else None
    session = Session(advisor, send=_ws_send, num_hands=num_hands, log_f=log_f, hh_f=hh_f)

    # 看门狗线程：被移出桌 / 桌散后服务端不再推任何消息，回调模型里没有别的唤醒点。
    stop_watchdog = threading.Event()

    def watchdog_loop():
        while not stop_watchdog.wait(30):
            session.watchdog_check()

    threading.Thread(target=watchdog_loop, daemon=True).start()

    def on_message(ws, raw):
        session.handle_message(ws, json.loads(raw))

    def on_error(ws, err):
        print(f"  [ws error] {err}", file=sys.stderr)

    def on_close(ws, code, reason):
        print(f"  [ws closed] code={code} reason={reason}", file=sys.stderr)

    header = [f"Authorization: Bearer {api_key}"]
    # 断线重连：最多重连若干次（§1：断线 120s 内 resync 补；这里简化为重连后继续）。
    attempts = 0
    while session.counters["hands"] < num_hands and attempts < 10:
        attempts += 1
        ws = websocket.WebSocketApp(
            WS_URL, header=header,
            on_message=on_message, on_error=on_error, on_close=on_close,
        )
        ws.run_forever(ping_interval=30, ping_timeout=10)
        if session.counters["hands"] < num_hands:
            print(f"  断线，3s 后重连（attempt {attempts}）…", file=sys.stderr)
            time.sleep(3)

    stop_watchdog.set()
    if log_f:
        log_f.close()
    if hh_f:
        hh_f.close()
    _report(session.counters)


def _street_name(s):
    if isinstance(s, str):
        return s
    # 数字街（0/1/2/3）→ 名称。
    return ["preflop", "flop", "turn", "river"][s] if isinstance(s, int) and 0 <= s <= 3 else "preflop"


def _report(counters):
    d = counters["decisions"]
    fb = counters["fallback"]
    print(f"hands={counters['hands']} decisions={d} "
          f"blueprint={counters['blueprint']} search={counters.get('search', 0)} fallback={fb} "
          f"({100.0 * fb / d if d else 0:.1f}% 兜底) "
          f"hh={counters.get('hh_hands', 0)}(+{counters.get('hh_skipped', 0)} skipped) "
          f"watchdog_rejoins={counters.get('watchdog_rejoins', 0)}",
          file=sys.stderr)


# ===========================================================================
# 离线 selftest（不连网，验 driver↔advisor IPC + 出合法动作 + HH byte-equal 隔离）
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

    # 场景 4（缺口②）：flop 首点未起注 + **真栈**（非对称深码 SB 600BB / BB 200BB）。验 driver 回推
    # hand-start 真栈 → request 带 stacks[6]；advisor 路径合法（开 --search 时走实时搜索 source=search /
    # search_giveup:*，否则 blueprint）。SB complete + BB check 在 nolimp/preopen 都是 on-tree（无 limp gap）。
    hand4 = HandState("h4", button_seat=0, my_seat=1)  # SB 先动 flop
    hand4.hole = ["Ah", "Kd"]
    for s in [3, 4, 5, 0]:
        hand4.on_player_action(s, "fold", None)
    hand4.on_player_action(1, "call", 20)  # SB complete
    hand4.on_player_action(2, "check", None)  # BB check → flop
    hand4.on_community(["7h", "2c", "Ks"], "flop")
    # your_turn.players[].stack 决策时 remaining（非对称）→ 滚动记录全 6 座。
    for s, stk in zip(range(NUM_SEATS), [2000, 12000, 4000, 2000, 2000, 2000]):
        hand4.update_stacks(s, stk)
    req4 = hand4.build_request(valid_flop)
    if "stacks" not in req4 or len(req4["stacks"]) != NUM_SEATS:
        raise RuntimeError(f"[stacks] 真栈全已知时 build_request 须带 stacks[6]，得 {req4.get('stacks')}")
    resp4 = advisor.decide(req4)
    _assert_legal(resp4, valid_flop, "flop+stacks")
    print(f"[selftest 4 flop+真栈] stacks={req4['stacks']} resp={resp4} "
          f"(--search 开则 source=search/search_giveup:*，否则 blueprint)", file=sys.stderr)

    # 场景 5（§4.2 HH 隔离）：canned 整手序列走 Session 真实消息路径，挂/不挂 --hh-log
    # 各跑一遍，advisor 请求/响应流 + 发出的 action 包必须逐字节一致；HH 行字段齐。
    _selftest_hh_byte_equal()

    print("OK: 5 个 canned 场景跑通：advisor 全程出合法动作、driver 组请求/动作包（含真栈 stacks）"
          "正常、HH 日志 byte-equal 隔离成立。", file=sys.stderr)


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


class _StubAdvisor:
    """HH 隔离测试用 stub：确定性合法动作（check→call→fold 优先级）+ 记录请求/响应字节。
    隔离测试比的是「driver 喂 advisor 的请求流是否被 HH 日志开关扰动」，不测策略——
    请求流 byte-equal ⟹ 同 seed 新起的真 advisor 输出也 byte-equal（advisor 无状态）。"""

    def __init__(self):
        self.transcript = []

    def decide(self, req):
        v = req["valid"]
        if v.get("can_check"):
            resp = {"action": "check", "source": "stub"}
        elif v.get("can_call"):
            resp = {"action": "call", "source": "stub"}
        else:
            resp = {"action": "fold", "source": "stub"}
        self.transcript.append((json.dumps(req, ensure_ascii=False), json.dumps(resp)))
        return resp


class _FakeWs:
    def close(self):
        pass


def _canned_hh_messages():
    """一手 6-max 打到摊牌的 canned 序列（button=0、我=BB 座 2）：preflop SB call/BB check →
    flop SB bet 20/BB call → turn/river 双 check → 摊牌我赢（AhKd 对 K > QsQd 对 Q）。
    覆盖：names / 全 6 座真栈 / actions_ext / 多街 board / shown_cards / winners。
    数字与 Rust 侧 `openpoker_hh` 单测的 canned 记录一致（两边互为 oracle）。"""
    def pa(seat, action, amount, street, stack, delta):
        return {"type": "player_action", "seat": seat, "action": action, "amount": amount,
                "amount_mode": "to_total" if action in ("raise", "bet") else None,
                "street": street, "stack": stack, "contribution_delta": delta}

    def players(stacks):
        return [{"seat": s, "name": f"bot{s}", "stack": st} for s, st in enumerate(stacks)]

    def your_turn(token, board, plist, valid_actions, min_raise, max_raise):
        m = {"type": "your_turn", "hand_id": "hh-selftest-1", "turn_token": token, "seat": 2,
             "players": plist, "valid_actions": valid_actions,
             "min_raise": min_raise, "max_raise": max_raise}
        if board is not None:
            m["community_cards"] = board
        return m

    b3 = ["7h", "2c", "Ks"]
    b4 = b3 + ["5d"]
    b5 = b4 + ["9c"]
    return [
        {"type": "hand_start", "hand_id": "hh-selftest-1", "seat": 2, "dealer_seat": 0,
         "table_id": "tbl-selftest-1", "blinds": {"small_blind": 10, "big_blind": 20}},
        {"type": "hole_cards", "cards": ["Ah", "Kd"]},
        pa(3, "fold", None, "preflop", 2000, 0),
        pa(4, "fold", None, "preflop", 2000, 0),
        pa(5, "fold", None, "preflop", 2000, 0),
        pa(0, "fold", None, "preflop", 2000, 0),
        pa(1, "call", 20, "preflop", 1980, 10),
        your_turn("tt-1", None, players([2000, 1980, 1980, 2000, 2000, 2000]),
                  [{"action": "check"}, {"action": "raise", "min": 40, "max": 2000}], 40, 2000),
        pa(2, "check", None, "preflop", 1980, 0),
        {"type": "community_cards", "cards": b3, "street": "flop"},
        pa(1, "bet", 20, "flop", 1960, 20),
        your_turn("tt-2", b3, players([2000, 1960, 1980, 2000, 2000, 2000]),
                  [{"action": "call", "amount": 20}, {"action": "raise", "min": 40, "max": 1980}],
                  40, 1980),
        pa(2, "call", 20, "flop", 1960, 20),
        {"type": "community_cards", "cards": b4, "street": "turn"},
        pa(1, "check", None, "turn", 1960, 0),
        your_turn("tt-3", b4, players([2000, 1960, 1960, 2000, 2000, 2000]),
                  [{"action": "check"}, {"action": "bet", "min": 20, "max": 1960}], 20, 1960),
        pa(2, "check", None, "turn", 1960, 0),
        {"type": "community_cards", "cards": b5, "street": "river"},
        pa(1, "check", None, "river", 1960, 0),
        your_turn("tt-4", b5, players([2000, 1960, 1960, 2000, 2000, 2000]),
                  [{"action": "check"}, {"action": "bet", "min": 20, "max": 1960}], 20, 1960),
        pa(2, "check", None, "river", 1960, 0),
        {"type": "hand_result", "pot": 80,
         "winners": [{"seat": 2, "stack": 2040, "amount": 80,
                      "hand_description": "pair of kings"}],
         "final_stacks": {"0": 2000, "1": 1960, "2": 2040, "3": 2000, "4": 2000, "5": 2000},
         "shown_cards": {"1": ["Qs", "Qd"], "2": ["Ah", "Kd"]}},
    ]


def _selftest_hh_byte_equal():
    """挂 / 不挂 --hh-log 跑同一 canned 序列（同走 Session 真实路径），advisor 请求/响应流 +
    发出的 action 包逐字节一致（HH 只准旁路落盘、不准扰动 advisor 输入）；HH 行字段齐。"""
    import os
    import tempfile

    def run_once(hh_path):
        stub = _StubAdvisor()
        sent = []
        hh_f = open(hh_path, "a") if hh_path else None
        session = Session(stub, send=lambda ws, obj: sent.append(json.dumps(obj)),
                          num_hands=10 ** 9, log_f=None, hh_f=hh_f)
        ws = _FakeWs()
        for m in _canned_hh_messages():
            session.handle_message(ws, m)
        if hh_f:
            hh_f.close()
        if session.state["hand"] is not None:
            raise RuntimeError("[hh] hand_result 后 hand 应清空")
        return stub.transcript, sent, session.counters

    t_off, sent_off, _ = run_once(None)
    with tempfile.TemporaryDirectory() as d:
        hh_path = os.path.join(d, "hh.jsonl")
        t_on, sent_on, counters_on = run_once(hh_path)
        with open(hh_path) as f:
            lines = [json.loads(line) for line in f]

    if t_off != t_on:
        raise RuntimeError("[hh] 挂/不挂 HH 日志 advisor 请求/响应流不 byte-equal")
    if sent_off != sent_on:
        raise RuntimeError("[hh] 挂/不挂 HH 日志 发出的 action 包不一致")
    if counters_on["hh_hands"] != 1 or len(lines) != 1:
        raise RuntimeError(f"[hh] 应落 1 行 HH，得 {len(lines)} 行 (counters={counters_on})")
    rec = lines[0]
    for k in ("hand_id", "button_seat", "my_seat", "hole", "board", "actions", "actions_ext",
              "names", "stacks_start", "committed_total", "hand_result", "table_id"):
        if k not in rec:
            raise RuntimeError(f"[hh] 记录缺字段 {k}")
    if rec["table_id"] != "tbl-selftest-1":
        raise RuntimeError(f"[hh] table_id 未落盘: {rec['table_id']!r}")
    if len(rec["actions"]) != 12 or len(rec["actions_ext"]) != 12:
        raise RuntimeError(f"[hh] actions/actions_ext 应各 12 条，得 "
                           f"{len(rec['actions'])}/{len(rec['actions_ext'])}")
    if rec["stacks_start"] != [2000] * NUM_SEATS:
        raise RuntimeError(f"[hh] 回推 hand-start 真栈错: {rec['stacks_start']}")
    if rec["names"].get("1") != "bot1" or rec["names"].get("5") != "bot5":
        raise RuntimeError(f"[hh] 对手 name 未捕到: {rec['names']}")
    if rec["board"] != ["7h", "2c", "Ks", "5d", "9c"]:
        raise RuntimeError(f"[hh] board 错: {rec['board']}")
    hr = rec["hand_result"]
    if hr.get("shown_cards", {}).get("1") != ["Qs", "Qd"] or not hr.get("winners"):
        raise RuntimeError(f"[hh] hand_result 摊牌/winners 未原样落盘: {hr}")
    # 决策点真用到了真栈（your_turn 后请求带 stacks[6]）：
    if '"stacks"' not in t_on[0][0]:
        raise RuntimeError("[hh] 首决策请求应带 stacks（全 6 座已观测）")
    print("[selftest 5 HH byte-equal] 挂/不挂 --hh-log：advisor 请求/响应 + action 包逐字节一致；"
          "HH 1 行字段齐（12 动作 + names + stacks_start + 摊牌原样）。", file=sys.stderr)


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
    p.add_argument("--hh-log", default="openpoker_hh.jsonl",
                   help="每手落全桌 HH JSONL（append 累积；§4.2 数据管道）；空串关闭")
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
            run_real(advisor, args.api_key, args.num_hands, action_log=log,
                     hh_log=args.hh_log if args.hh_log else None)
    finally:
        advisor.close()


if __name__ == "__main__":
    main()

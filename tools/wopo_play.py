#!/usr/bin/env python3
"""wopo 牌桌服务 driver：只收不发，hero 的建议动作写进行动文件（外部程序读）。

分层（tools/poker_live/）：
  通讯        本文件 run_ws / run_replay：被动连 WS（断线重连）或读回放文件，原始消息交给会话
  牌桌校验    table.HandEngine：自跑一份牌局，逐条核对服务器消息；对不上 → 这手作废，等下一手
  打牌决策    decision.DecisionWorker + advisor_client.Advisor：常驻 Rust advisor（同 openpoker_play）
  会话        wopo.WopoSession：wopo 消息适配、轮到 hero 触发决策、写行动文件

与 openpoker_play 的区别：
  - 不 join、不发任何消息；中途接入时略过当前手，从下一个 hand_start 开始跟。
  - 没有 your_turn：table_state.actor_seat == hero 座（恒为 0）即轮到 hero，合法区间由牌桌层自己算。
  - hero 的动作不发服务器，写行动文件（JSONL 追加，一个决策点一行：建议动作 + 策略分布 probs）。
    hero 实际动作以服务器回来的 player_action 为准（可能和建议不同）。决策在后台线程跑：搜索没
    算完 hero 就已经出牌 → 结果过期，不写行动文件（只进 --action-log，标 stale）。
  - --seats（默认 9）= solver 座位数，blueprint 须同座位数；桌子本身可以是 3..--seats 座（座位号 0..k-1，
    多出来的 solver 座当空座），至少 3 人发牌，否则略过该手。
  - advisor 的建议先过一遍合法性：不能加注时的 all_in 改成 call（source 加 :allin_as_call），其余不在
    合法区间里的动作改安全动作（fallback:illegal_advisor_action，action-log 的 resp.rejected 记原回复）。
  - --exploit-4bet（PM-17）同 poker_manager：请求带玩家名、每手 observe 带投入金额。
  - --hh-log：每手打完且没作废的牌局写一行（opponent_analysis 的 wopo 平台读）。
  - --play-log：给人看的牌桌实况（手牌 / 公共牌 / 玩家行动 / hero 决策与实际行动 / 结算 / 作废），
    诊断信息（advisor 启动、连接、过期、未知消息）仍走 stderr。
  - --ante（桌面筹码，默认 0）：开局每个发牌座先交 ante 再发盲注，牌桌层按此核对底池 / 栈（快照 bet
    只含盲注；对不上 → 作废）。--blueprint-ante（solver 筹码，BB = 100，= train_cfr --ante）透传给
    advisor 加载对应 blueprint；每手要求 ante × 100 == blueprint-ante × 大盲，否则这手作废
    （ante_mismatch）。短桌空座在实时搜索里不交 ante（树里是按满桌人人交训的，翻前查表照用）。
  - 连 WS 带 token 鉴权（Authorization: Bearer）：环境变量 WOPO_TOKEN 优先，其次项目根 .env 的
    WOPO_TOKEN；不走命令行（免得进 ps / shell 历史）。服务端拒绝（关闭码 4001 / 握手 401、403）→
    不重连，直接退出。

行动文件每行字段：ts / hand_id / table_id / table_seq（触发决策的那条 table_state）/ street / hole /
board / pot / to_call / valid（can_check/can_call/can_raise/min_raise/max_raise，金额 = 本街总额）/
action（fold/check/call/raise/all_in）/ amount（raise 的本街总额，其余 null）/ probs（策略分布
[[动作, 概率]]，兜底动作为 null）/ chosen / source / decide_ms。

用法：
  wopo_play.py [--url ws://HOST:PORT/ws] --action-file hero_actions.jsonl \\
      --cropped-checkpoint X.crop --bucket-table B.bin --search --search-trigger all-postflop ...
  wopo_play.py --replay session.log ...   回放 WS 日志（原始消息逐行，或 --ws-log 格式）
  wopo_play.py --selftest                 离线自测（stub advisor，不需要 blueprint）
"""

import argparse
import io
import json
import os
import shlex
import sys
import tempfile
import threading
import time

from poker_live.advisor_client import (add_advisor_args, add_fourbet_args, advisor_extra_args,
                                       exploit_enabled, fourbet_enabled, fourbet_extra_args,
                                       start_advisor, validate_advisor_args, validate_fourbet_args)
from poker_live.decision import DecisionWorker
from poker_live.table import HandEngine, HandInvalid
from poker_live.wopo import ActionFile, WopoSession, fmt_probs, legalize
from poker_live.wslog import WsLog

HERO_SEAT = 0
DEFAULT_URL = "ws://64.176.55.133:3000/ws"
TOKEN_ENV = "WOPO_TOKEN"
ENV_FILE = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), ".env")
AUTH_CLOSE_CODE = 4001  # 服务端鉴权失败的关闭码（先回 {"type":"error","code":"auth_failed"}）


def load_token(env_file=ENV_FILE):
    """wopo 服务端 token：环境变量 WOPO_TOKEN 优先，其次 env_file 里的 WOPO_TOKEN（只认这一个键，
    不执行 shell；重复出现以最后一行为准）。缺失 / 空 / 含空白 / 引号不配对 → None。"""
    token = os.environ.get(TOKEN_ENV)
    if token is None and os.path.isfile(env_file):
        with open(env_file) as f:
            for line in f:
                line = line.strip()
                if line.startswith("export "):
                    line = line[7:].lstrip()
                name, sep, value = line.partition("=")
                if sep and name.strip() == TOKEN_ENV:
                    try:
                        parts = shlex.split(value, comments=True)
                    except ValueError:
                        parts = []
                    token = parts[0] if len(parts) == 1 else ""
    if not token or any(c.isspace() for c in token):
        return None
    return token


# ===========================================================================
# 通讯
# ===========================================================================
def run_ws(session, url, token, ws_log=None, reconnect_wait=3):
    """被动连 wopo WS（带 token）：收到的原始消息交给 session；断线 → session.on_disconnect + 等几秒
    重连。打满 --num-hands、Ctrl-C 或鉴权被拒结束；鉴权被拒返回 False，其余 True。"""
    import websocket  # 延迟 import：回放 / selftest 不需要 websocket-client

    stopping = [False]
    auth_failed = [False]

    def on_open(ws):
        print(f"  [ws open] {url}", file=sys.stderr)
        if ws_log:
            ws_log.event("open")

    def on_message(ws, raw):
        if ws_log:
            ws_log.msg("in", raw)
        session.on_raw(raw)
        if session.done:
            print("  打满 --num-hands，离场。", file=sys.stderr)
            stopping[0] = True
            ws.close()

    def on_error(ws, err):
        print(f"  [ws error] {err!r}", file=sys.stderr)
        if isinstance(err, KeyboardInterrupt):
            stopping[0] = True  # run_forever 吞掉 Ctrl-C 后正常返回 → 不标记就会被当成断线重连
        if getattr(err, "status_code", None) in (401, 403):
            auth_failed[0] = True  # 握手阶段就被拒（HTTP 层鉴权的服务端）
        if ws_log:
            ws_log.event("error", error=repr(err))

    def on_close(ws, code, reason):
        print(f"  [ws closed] code={code} reason={reason}", file=sys.stderr)
        if code == AUTH_CLOSE_CODE:
            auth_failed[0] = True
        if ws_log:
            ws_log.event("close", code=code,
                         reason=reason if isinstance(reason, str) else repr(reason))

    while True:
        if ws_log:
            ws_log.conn += 1
        ws = websocket.WebSocketApp(url, header=[f"Authorization: Bearer {token}"],
                                    on_open=on_open, on_message=on_message,
                                    on_error=on_error, on_close=on_close)
        try:
            ws.run_forever(ping_interval=30, ping_timeout=10)
        except KeyboardInterrupt:
            stopping[0] = True
        session.on_disconnect()
        if auth_failed[0]:
            print(f"  服务端拒绝 token（检查 {TOKEN_ENV}），不重连。", file=sys.stderr)
            return False
        if stopping[0] or session.done:
            return True
        print(f"  断线，{reconnect_wait}s 后重连…", file=sys.stderr)
        try:
            time.sleep(reconnect_wait)
        except KeyboardInterrupt:
            return True


def iter_replay(path):
    """回放文件 → 原始消息串。两种格式：每行一条原始消息；或 --ws-log 格式（dir=in 取 raw，
    dir=event 的 close 当断线）。产出 ("msg", raw) / ("disconnect", None)。"""
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                rec = json.loads(line)
            except ValueError:
                yield "msg", line
                continue
            if isinstance(rec, dict) and "dir" in rec and "type" not in rec:
                if rec.get("dir") == "in":
                    yield "msg", rec.get("raw")
                elif rec.get("dir") == "event" and rec.get("event") == "close":
                    yield "disconnect", None
            else:
                yield "msg", line


def run_replay(session, path):
    for kind, raw in iter_replay(path):
        if kind == "disconnect":
            session.on_disconnect()
        else:
            session.on_raw(raw)
        if session.done:
            break


# ===========================================================================
# 离线 selftest（stub advisor；用 tools/testdata 下的 wopo 实测日志）
# ===========================================================================
FIXTURE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "testdata",
                       "wopo_session_2026_10_16.jsonl")
# 上面这份实测日志对应的 --hh-log 输出；opponent_analysis 的 wopo 适配器测试读它。
HH_FIXTURE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "opponent_analysis",
                          "tests", "data", "wopo_hh_2026_10_16.jsonl")
# 同一会话改写成每人 ante 2（盲注 1/2）后的 --hh-log 输出（_selftest_ante 生成并核对）。
HH_ANTE_FIXTURE = os.path.join(os.path.dirname(HH_FIXTURE), "wopo_hh_ante_2026_10_27.jsonl")
# wopo ante-table（8 座、50/100、ante 100）2026-10-28 实盘里有人补盲的 4 手原始消息（修前全部作废），
# 及其 --hh-log 输出（_selftest_posts 生成并核对；opponent_analysis 的补盲回放测试读它）。
POSTS_FIXTURE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "testdata",
                             "wopo_posts_2026_10_28.jsonl")
HH_POSTS_FIXTURE = os.path.join(os.path.dirname(HH_FIXTURE), "wopo_hh_posts_2026_10_28.jsonl")


class _StubAdvisor:
    """确定性合法动作（check → call → fold）+ 记录请求。gate 非 None 时 decide 阻塞到 gate 被 set
    （模拟搜索慢于 hero 出牌）。"""

    def __init__(self, gate=None, reply=None):
        self.requests = []
        self.side = []
        self.gate = gate
        self.reply = reply  # reply(req) → 响应 dict 或 None（None = 走默认确定性动作）

    def decide(self, req):
        if self.gate is not None:
            self.gate.wait(10)
        self.requests.append(req)
        if self.reply is not None:
            r = self.reply(req)
            if r is not None:
                return r
        v = req["valid"]
        act = "check" if v.get("can_check") else "call" if v.get("can_call") else "fold"
        return {"action": act, "source": "stub", "probs": [[act, 1.0]], "chosen": act}

    def prewarm(self, req):
        self.side.append(("prewarm", req))

    def observe(self, rec):
        self.side.append(("observe", rec))


def _fixture_msgs():
    with open(FIXTURE) as f:
        return [json.loads(line) for line in f if line.strip()]


def _run_msgs(msgs, gate=None, sync=True, hook=None, exploit=False, prewarm=False,
              exploit_4bet=False, reply=None, hh_log=None, play_log=None, ante=0, blueprint_ante=0,
              n_seats=9):
    """msgs（dict 或 ("disconnect",)）喂一个新会话。返回 (session, stub, 行动文件行)。
    hook(i, session) 在每条消息后调用（异步场景用来在指定位置放行 advisor）。"""
    stub = _StubAdvisor(gate, reply)
    worker = DecisionWorker(stub, sync=sync)
    with tempfile.TemporaryDirectory() as d:
        path = os.path.join(d, "actions.jsonl")
        af = ActionFile(path)
        session = WopoSession(worker, af, n_seats=n_seats, hero_seat=HERO_SEAT,
                              exploit=exploit, prewarm=prewarm, exploit_4bet=exploit_4bet,
                              hh_log=hh_log, play_log=play_log, ante=ante,
                              blueprint_ante=blueprint_ante)
        for i, m in enumerate(msgs):
            if m == ("disconnect",):
                session.on_disconnect()
            else:
                session.on_raw(json.dumps(m))
            if hook:
                hook(i, session)
        worker.close(timeout=10)
        af.close()
        with open(path) as f:
            rows = [json.loads(line) for line in f]
    return session, stub, rows


def _check(cond, what):
    if not cond:
        raise RuntimeError(f"[selftest] {what}")


def _selftest_full_replay():
    msgs = _fixture_msgs()
    play = io.StringIO()
    session, stub, rows = _run_msgs(msgs, exploit=True, prewarm=True, play_log=play)
    c = session.counters
    # 日志：8 个 hand_start，前 7 手打完，第 14 手截断在中间（没有作废原因，只是没打完）。
    _check(c["hands"] == 8 and c["hands_complete"] == 7, f"手数 {c}")
    _check(not session.skip_reasons, f"实测日志不该有作废手: {session.skip_reasons}")
    _check(c["result_notes"] == 0, "结算核对（筹码守恒 / 座位集）应全过")
    # 每个 actor_seat=0 的快照（按 hand_id + 已发生动作数去重）恰好决策一次、全部写进行动文件。
    turns, n_act = set(), {}
    for m in msgs:
        h = m.get("hand_id")
        if m["type"] == "hand_start":
            n_act[h] = 0
        elif m["type"] == "player_action":
            n_act[h] += 1
        elif m["type"] == "table_state" and m.get("actor_seat") == HERO_SEAT and h in n_act:
            turns.add((h, n_act[h]))
    _check(c["decisions"] == len(turns) == len(rows) == c["written"],
           f"决策数 {c['decisions']} / hero 轮次 {len(turns)} / 行动文件 {len(rows)}")
    # hero 每个实际动作都有建议，且在牌桌层给的合法区间内。
    hero_acts = [m for m in msgs if m["type"] == "player_action" and m["seat"] == HERO_SEAT]
    _check(c["hero_actions"] == len(hero_acts) and c["hero_unadvised"] == 0,
           f"hero 动作对照 {c}")
    for row, act in zip(rows, hero_acts):
        v = row["valid"]
        a = act["action"]
        ok = (a == "fold" or (a == "check" and v["can_check"]) or (a == "call" and v["can_call"])
              or (a == "raise" and v["can_raise"] and v["min_raise"] <= act["amount"] <= v["max_raise"]))
        _check(ok and row["hand_id"] == act["hand_id"], f"hero 实际动作 {act} 不在区间 {v}")
        _check(row["to_call"] == (act["contribution_delta"] if a == "call" else row["to_call"]),
               f"to_call {row['to_call']} ≠ 实际跟注 {act['contribution_delta']}")
    # 请求内容：9 座桌 4 人发牌 → dealt_seats；开局真栈；盲注 1/2。
    r0 = stub.requests[0]
    _check(r0["num_seats"] == 9 and r0["dealt_seats"] == [0, 1, 2, 3]
           and r0["small_blind"] == 1 and r0["big_blind"] == 2, f"请求头 {r0}")
    _check(r0["stacks"][:4] == [200, 200, 200, 200] and r0["stacks"][4:] == [200] * 5,
           f"hand 7 开局栈 {r0['stacks']}")
    # hand 8：seat2 加注到 6（加注幅度 4）→ hero 面对再加注的最小到额 = 10、最大 = 2+198=200。
    h8 = [(q, row) for q, row in zip(stub.requests, rows) if row["hand_id"].endswith("000008")]
    q, row = h8[1]
    _check(row["valid"] == {"can_check": False, "can_call": True, "can_raise": True,
                            "min_raise": 10, "max_raise": 200}, f"hand 8 第 2 决策区间 {row['valid']}")
    _check(q["actions"][-2:] == [{"seat": 2, "action": "raise", "to": 6},
                                 {"seat": 3, "action": "fold"}], f"hand 8 历史 {q['actions']}")
    # play log：每手一个开局行 + 底牌 + 座位；每个决策 / hero 动作 / 对手动作 / 发牌 / 结算各一行。
    lines = [ln.split(" ", 1)[1] for ln in play.getvalue().splitlines()]
    count = lambda pred: sum(1 for ln in lines if pred(ln))
    n_type = lambda t: sum(m["type"] == t for m in msgs)
    opp_acts = sum(m["type"] == "player_action" and m["seat"] != HERO_SEAT for m in msgs)
    _check(count(lambda ln: ln.startswith("────")) == n_type("hand_start")
           and count(lambda ln: ln.startswith("底牌 ")) == n_type("hole_cards")
           and count(lambda ln: ln.startswith("座位 ")) == n_type("hand_start")
           and count(lambda ln: ln.startswith(">> 轮到 hero")) == c["decisions"]
           and count(lambda ln: ln.startswith(">> 决策  来源 ")) == c["decisions"]
           and count(lambda ln: "← hero 实际行动" in ln) == len(hero_acts)
           and count(lambda ln: ln.startswith(("翻牌 ", "转牌 ", "河牌 "))) == n_type("community_cards")
           and count(lambda ln: ln.startswith("seat") and "←" not in ln) == opp_acts
           and count(lambda ln: ln.startswith("结算 ")) == c["hands_complete"]
           and len(lines) == 3 * n_type("hand_start") + 2 * c["decisions"] + len(hero_acts)
           + n_type("community_cards") + opp_acts + c["hands_complete"],
           "play log 行数不对:\n" + play.getvalue())
    _check(all(ln.endswith(f"=(100)  建议 {ln.rsplit('建议 ', 1)[1]}") for ln in lines
               if ln.startswith(">> 决策")), "play log 决策行应带策略分布（stub 给 100%）")
    _check(fmt_probs([["fold", 0.0184], ["call", 0.9151], ["raise1pot", 0.0665], ["allin", 0.0]])
           == "(fold,call,1pot,allin)=(2,92,7,0)"
           and fmt_probs([["check", 0.4428], ["bet0.5pot", 0.5566], ["bet1pot", 0.0002]])
           == "(check,0.5pot,1pot)=(44,56,0)" and fmt_probs(None) == "", "策略分布格式")
    _check("seat1 Shaggyx bet 6" in lines and "seat2 hsjwnbxj raise 到 6" in lines,
           "play log：没人下注时的 raise 应写成 bet，面对下注的写 raise 到")
    _check(q["stacks"][:4] == [200, 175, 175, 250], f"hand 8 开局栈 {q['stacks'][:4]}")
    # 行动文件字段齐（外部程序读的接口）。
    keys = {"ts", "hand_id", "table_id", "table_seq", "street", "hole", "board", "pot", "to_call",
            "valid", "action", "amount", "probs", "chosen", "source", "decide_ms"}
    _check(all(set(r) == keys for r in rows), f"行动文件字段 {sorted(rows[0])}")
    # exploit：每手打完发 observe；prewarm：hero 未弃牌的街起点发。
    _check(sum(1 for k, _ in stub.side if k == "observe") == 7, "observe 每手一条")
    print(f"[selftest 1 实测日志回放] {c['hands']} 手 / {len(rows)} 个决策点全部写入、"
          f"hero 实际动作均在区间内", file=sys.stderr)


def _selftest_mid_hand_join():
    msgs = _fixture_msgs()
    start = next(i for i, m in enumerate(msgs) if m.get("hand_id", "").endswith("000008")) + 5
    session, _, rows = _run_msgs(msgs[start:])
    _check(all(not r["hand_id"].endswith("000008") for r in rows), "中途接入的那手不该决策")
    _check(session.counters["hands"] == 6 and not session.skip_reasons,
           f"中途接入后从下一手正常跟: {session.counters} {session.skip_reasons}")
    print("[selftest 2 中途接入] 当前手略过，从下一个 hand_start 开始", file=sys.stderr)


def _tamper_case(mutate, want_tag, label):
    """改一条消息 → 那手按 want_tag 作废、不再决策，其余手不受影响。"""
    msgs = _fixture_msgs()
    hid = "local-table:hand:000009"
    msgs, victim = mutate(msgs, hid)
    session, _, rows = _run_msgs(msgs)
    _check(session.skip_reasons == {want_tag: 1}, f"{label}: 作废原因 {session.skip_reasons}")
    late = [r for r in rows if r["hand_id"] == hid and r["table_seq"] > victim]
    _check(not late, f"{label}: 作废后不该再决策 {late}")
    _check(any(r["hand_id"].endswith("000010") for r in rows), f"{label}: 下一手应正常")


def _selftest_tamper():
    def bad_pot(msgs, hid):
        i = next(i for i, m in enumerate(msgs) if m.get("hand_id") == hid
                 and m["type"] == "player_action" and m["seat"] != HERO_SEAT)
        msgs[i] = dict(msgs[i], pot_before=msgs[i]["pot_before"] + 1)
        return msgs, msgs[i]["table_seq"]

    def drop(msgs, hid):
        i = next(i for i, m in enumerate(msgs) if m.get("hand_id") == hid
                 and m["type"] == "player_action")
        return msgs[:i] + msgs[i + 1:], msgs[i]["table_seq"]

    def bad_actor(msgs, hid):
        i = next(i for i, m in enumerate(msgs) if m.get("hand_id") == hid
                 and m["type"] == "table_state" and m.get("actor_seat") not in (None, HERO_SEAT))
        msgs[i] = dict(msgs[i], actor_seat=HERO_SEAT)
        return msgs, msgs[i]["table_seq"] - 1

    def short_handed(msgs, hid):
        i = next(i for i, m in enumerate(msgs) if m.get("hand_id") == hid
                 and m["type"] == "table_state")
        seats = [dict(s, in_hand=False) if s["seat"] in (2, 3) else s for s in msgs[i]["seats"]]
        msgs[i] = dict(msgs[i], seats=seats)
        return msgs, msgs[i]["table_seq"] - 1

    def disconnect(msgs, hid):
        i = next(i for i, m in enumerate(msgs) if m.get("hand_id") == hid
                 and m["type"] == "community_cards")
        return msgs[:i] + [("disconnect",)] + msgs[i:], msgs[i]["table_seq"] - 1

    _tamper_case(bad_pot, "pot_before", "底池对不上")
    _tamper_case(drop, "table_seq_gap", "漏一条消息")
    _tamper_case(bad_actor, "snapshot_actor", "行动者对不上")
    _tamper_case(short_handed, "short_handed", "只发 2 人")
    _tamper_case(disconnect, "disconnected", "断线")
    print("[selftest 3 消息对不上] 底池 / 漏消息 / 行动者 / 两人局 / 断线 → 该手作废，下一手正常",
          file=sys.stderr)


def _selftest_stale():
    """异步：advisor 卡住期间 hero 实际动作先到 → 结果过期不写；之后的决策照常写。"""
    msgs = _fixture_msgs()
    gate = threading.Event()
    first_hero_act = next(i for i, m in enumerate(msgs)
                          if m["type"] == "player_action" and m["seat"] == HERO_SEAT)

    def hook(i, session):
        if i == first_hero_act:
            gate.set()
        if i >= first_hero_act:
            # 放行后模拟「advisor 比 hero 快」：每条消息后等已提交的决策都回来。
            deadline = time.time() + 10
            while session.counters["decisions"] < session.counters["turns"] and time.time() < deadline:
                time.sleep(0.001)

    session, _, rows = _run_msgs(msgs, gate=gate, sync=False, hook=hook)
    c = session.counters
    _check(c["stale"] == 1 and c["hero_unadvised"] == 1, f"过期计数 {c}")
    _check(c["written"] == c["decisions"] - 1 == len(rows), f"写入数 {c}")
    _check(rows[0]["table_seq"] > msgs[first_hero_act]["table_seq"], "过期结果不该进行动文件")
    print("[selftest 4 决策过期] 搜索期间 hero 已出牌 → 不写行动文件；后续决策正常", file=sys.stderr)


def _selftest_engine_rules():
    """规则细节：不完整全下加注不重开加注权、全下跟不满、最小下注 / 加注幅度。"""
    e = HandEngine("h", 0, 1, 2, HERO_SEAT, 4)
    seats = {s: {"seat": s, "stack": st, "bet": b, "in_hand": True, "folded": False}
             for s, st, b in [(0, 100, 0), (1, 99, 1), (2, 98, 2), (3, 15, 0)]}
    e.apply_hole(["Ah", "Kd"])
    e.apply_snapshot({"street": "preflop", "board": [], "pot": 3, "actor": 3, "seats": seats,
                      "n_seats": 4})

    def act(seat, action, delta, to=None):
        st, pot = e.stack[seat], e.pot
        e.apply_action({"seat": seat, "action": action, "street": e.street, "delta": delta,
                        "to": to, "stack_before": st, "stack_after": st - delta,
                        "pot_before": pot, "pot_after": pot + delta})

    act(3, "call", 2)
    v = e.hero_turn()
    _check(v["min_raise"] == 4 and v["max_raise"] == 100, f"开池最小加注到 4: {v}")
    act(0, "raise", 10, to=10)                 # hero 加注到 10（幅度 8）
    act(1, "fold", 0)
    act(2, "call", 8)
    act(3, "raise", 13, to=15)                 # 全下 15：幅度 5 < 8，不完整加注
    _check(e.actions[-1] == {"seat": 3, "action": "all_in", "to": 15}, f"{e.actions[-1]}")
    v = e.hero_turn()
    _check(v == {"can_check": False, "can_call": True, "can_raise": False,
                 "min_raise": None, "max_raise": None}, f"不完整加注后 hero 无加注权: {v}")
    try:
        act(0, "raise", 20, to=30)
        raise RuntimeError("[selftest] 不完整加注后再加注应判非法")
    except HandInvalid as ex:
        _check(str(ex).startswith("raise_not_reopened"), str(ex))
    print("[selftest 5 规则细节] 不完整全下加注 / 加注权 / 最小加注幅度", file=sys.stderr)


def _selftest_legalize():
    """advisor 建议的合法性：不能加注时 all_in → call；其余越界动作 → 安全动作兜底。"""
    call_only = {"can_check": False, "can_call": True, "can_raise": False,
                 "min_raise": None, "max_raise": None}
    check_only = dict(call_only, can_check=True, can_call=False)
    open_raise = {"can_check": False, "can_call": True, "can_raise": True,
                  "min_raise": 10, "max_raise": 200}
    r = legalize({"action": "all_in", "source": "blueprint", "probs": [["all_in", 1.0]]}, call_only)
    _check(r == {"action": "call", "source": "blueprint:allin_as_call", "probs": [["all_in", 1.0]]},
           f"对手已全下时 all_in 应改 call: {r}")
    r = legalize({"action": "all_in", "source": "search"}, open_raise)
    _check(r == {"action": "all_in", "source": "search"}, f"能加注时 all_in 照写: {r}")
    r = legalize({"action": "all_in", "source": "blueprint"}, check_only)
    _check(r == {"action": "check", "source": "fallback:illegal_advisor_action",
                 "rejected": {"action": "all_in", "amount": None, "source": "blueprint"}},
           f"既不能加注也不能跟注 → 兜底: {r}")
    for bad in ({"action": "raise", "amount": 8}, {"action": "raise", "amount": 201},
                {"action": "raise", "amount": 12.5}, {"action": "raise", "amount": True},
                {"action": "check"}, {"action": "bogus"}):
        r = legalize(dict(bad, source="x"), open_raise)
        _check(r["action"] == "fold" and r["source"] == "fallback:illegal_advisor_action",
               f"越界 {bad} 应兜底: {r}")
    for good in ({"action": "raise", "amount": 10}, {"action": "raise", "amount": 200},
                 {"action": "call"}, {"action": "fold"}):
        _check(legalize(dict(good, source="x"), open_raise) == dict(good, source="x"),
               f"合法 {good} 不该改")
    # 接进会话：advisor 每次回 all_in → 行动文件里不能加注的点写 call、能加注的点照写 all_in。
    _, _, rows = _run_msgs(_fixture_msgs(), reply=lambda req: {"action": "all_in", "source": "stub"})
    _check(rows and all(r["action"] == ("all_in" if r["valid"]["can_raise"] else "call")
                        for r in rows if r["valid"]["can_raise"] or r["valid"]["can_call"]),
           f"行动文件 all_in 处理 {[(r['action'], r['valid']) for r in rows]}")
    print("[selftest 6 建议合法性] 不能加注的 all_in → call；越界动作 → 安全动作", file=sys.stderr)


def _selftest_exploit_4bet():
    """PM-17：请求带玩家名、每手 observe 带投入金额；只开 --exploit 时 observe 与旧格式相同。"""
    msgs = _fixture_msgs()
    session, stub, _ = _run_msgs(msgs, exploit_4bet=True)
    _check(all(q.get("names") and q["names"][HERO_SEAT] == "hero" for q in stub.requests),
           "PM-17 开时请求要带玩家名")
    obs = [rec for k, rec in stub.side if k == "observe"]
    _check(len(obs) == session.counters["hands_complete"] == 7, f"observe 每手一条: {len(obs)}")
    # 和服务器原始 player_action 逐条对：投入 = contribution_delta；加注投入 > 动作前跟注额。
    by_hand = {}
    for m in msgs:
        if m["type"] == "player_action":
            by_hand.setdefault(m["hand_id"], []).append(m)
    hand_ids = [m["hand_id"] for m in msgs if m["type"] == "hand_result"]
    for hid, rec in zip(hand_ids, obs):
        raw = by_hand[hid]
        _check(len(rec["actions"]) == len(raw), f"{hid} observe 动作数")
        for a, m in zip(rec["actions"], raw):
            _check(a["contribution_delta"] == m["contribution_delta"], f"{hid} 投入 {a} vs {m}")
            if a["action"] in ("raise", "all_in"):
                _check(a["contribution_delta"] > a["to_call_before"], f"{hid} 加注应超过跟注额 {a}")
            elif a["action"] == "call":
                _check(a["contribution_delta"] == a["to_call_before"], f"{hid} 跟注额 {a}")
    # 只开 --exploit：observe 不带金额（与改动前 IPC 字节相同）；两个都不开：不带名字、不发 observe。
    _, stub, _ = _run_msgs(msgs, exploit=True)
    _check(all(set(a) == {"seat", "street", "action"}
               for k, rec in stub.side if k == "observe" for a in rec["actions"]),
           "只开 --exploit 时 observe 不该带金额")
    _, stub, _ = _run_msgs(msgs)
    _check(not stub.side and all("names" not in q for q in stub.requests), "剥削全关时不带名字、不发 observe")
    print("[selftest 7 PM-17] 请求带名字、observe 带投入金额并与服务器逐条一致", file=sys.stderr)


def _selftest_hh_log():
    """--hh-log：每手打完且没作废的牌局写一行；作废的手不写。"""

    msgs = _fixture_msgs()
    buf = io.StringIO()
    session, _, _ = _run_msgs(msgs, hh_log=buf)
    rows = [json.loads(line) for line in buf.getvalue().splitlines()]
    _check(len(rows) == session.counters["hands_complete"] == 7, f"hh 行数 {len(rows)}")
    results = [m for m in msgs if m["type"] == "hand_result"]
    for row, res in zip(rows, results):
        _check(row["wopo_hh"] == 1 and row["hand_id"] == res["hand_id"] and row["result"] == res
               and row["result_notes"] == [] and row["my_seat"] == HERO_SEAT, f"hh 行 {row['hand_id']}")
        start = {int(k): v for k, v in row["initial_stacks"].items()}
        final = {int(k): v for k, v in res["final_stacks"].items()}
        _check(sorted(start) == row["dealt_seats"] == sorted(final)
               and sum(start.values()) == sum(final.values()), f"{row['hand_id']} 开局栈 {start}")
        # 投入合计 = 服务器最后报的底池（没人跟的注结算时退回，不能和 total_pot 比）。
        last_pot = [m["pot"] for m in msgs if m["type"] == "player_action"
                    and m["hand_id"] == res["hand_id"]][-1]
        _check(sum(a["contribution_delta"] for a in row["actions"]) + row["small_blind"]
               + row["big_blind"] == last_pot, f"{row['hand_id']} 投入合计 ≠ 底池")
    # 与 opponent_analysis 测试用的样例逐行相同（改了 hh 格式要同步重生成样例、改适配器）。
    with open(HH_FIXTURE) as f:
        _check(rows == [json.loads(line) for line in f if line.strip()],
               f"hh 输出与 {HH_FIXTURE} 不一致")
    # 作废的手（底池对不上）不写。
    hid = "local-table:hand:000009"
    i = next(i for i, m in enumerate(msgs) if m.get("hand_id") == hid
             and m["type"] == "player_action" and m["seat"] != HERO_SEAT)
    msgs[i] = dict(msgs[i], pot_before=msgs[i]["pot_before"] + 1)
    buf = io.StringIO()
    _run_msgs(msgs, hh_log=buf)
    ids = [json.loads(line)["hand_id"] for line in buf.getvalue().splitlines()]
    _check(len(ids) == 6 and hid not in ids, f"作废的手不该写 hh: {ids}")
    print("[selftest 8 hh 日志] 打完的手逐手写入、投入合计 = 底池；作废的手不写", file=sys.stderr)


def _selftest_table_size():
    """--seats 9 的 solver 接小桌：实测会话（9 座桌，座 0-3 发牌）的 table_state 裁成 6 / 8 座，
    决策请求和 9 座时逐个相同（物理座 = solver 座，多出来的当空座）；超出 / 不连续 / 中途变 → 作废。"""
    msgs = _fixture_msgs()

    def resize(fn, only=None):
        out = []
        for i, m in enumerate(msgs):
            if m["type"] == "table_state" and (only is None or i in only):
                m = dict(m, seats=fn(m["seats"]))
            out.append(m)
        return out

    _, base_stub, base_rows = _run_msgs(msgs)
    strip = lambda rows: [{k: v for k, v in r.items() if k not in ("ts", "decide_ms")} for r in rows]
    for k in (6, 8):
        session, stub, rows = _run_msgs(resize(lambda seats: seats[:k]))
        _check(not session.skip_reasons, f"{k} 座桌不该作废: {session.skip_reasons}")
        _check(stub.requests == base_stub.requests and strip(rows) == strip(base_rows),
               f"{k} 座桌的请求 / 行动文件应与 9 座时相同")
    n_hands = sum(m["type"] == "hand_start" for m in msgs)
    ten = resize(lambda seats: seats + [dict(seats[-1], seat=9)])
    gap = resize(lambda seats: [x for x in seats if x["seat"] != 5])
    for name, bad, tag in (("10 座桌", ten, "seat_count"), ("座位号不连续", gap, "seat_numbers")):
        session, _, rows = _run_msgs(bad)
        _check(session.skip_reasons == {tag: n_hands} and not rows,
               f"{name}应每手作废（{tag}）: {session.skip_reasons}")
    # 手中途座位数变了 → 只作废这一手。
    hid = msgs[0]["hand_id"]
    idx = [i for i, m in enumerate(msgs) if m["type"] == "table_state" and m["hand_id"] == hid]
    session, _, _ = _run_msgs(resize(lambda seats: seats[:6], only=set(idx[1:2])))
    _check(session.skip_reasons == {"seat_count_changed": 1}, f"中途变座位数: {session.skip_reasons}")
    print("[selftest 10 桌子座位数] 6 / 8 座桌按 9 座 solver 照常决策（请求逐个相同）；10 座 / 座位号不连续 / "
          "中途变 → 作废", file=sys.stderr)


def _selftest_token():
    saved = os.environ.pop(TOKEN_ENV, None)
    try:
        with tempfile.TemporaryDirectory() as d:
            env = os.path.join(d, ".env")
            cases = [("API_KEY=x\nWOPO_TOKEN=abc123\n", "abc123"),
                     ("export WOPO_TOKEN='abc 123'\n", None),         # 含空白
                     ("WOPO_TOKEN=\"q1\"  # comment\n", "q1"),
                     ("WOPO_TOKEN=a\nWOPO_TOKEN=b\n", "b"),           # 最后一行为准
                     ("WOPO_TOKEN=\n", None), ("WOPO_TOKEN='abc\n", None),
                     ("WOPO_TOKENX=abc\n", None), (None, None)]
            for text, want in cases:
                if text is not None:
                    with open(env, "w") as f:
                        f.write(text)
                elif os.path.exists(env):
                    os.remove(env)
                got = load_token(env)
                _check(got == want, f".env {text!r}: token={got!r} 应为 {want!r}")
            with open(env, "w") as f:
                f.write("WOPO_TOKEN=from_file\n")
            os.environ[TOKEN_ENV] = "from_env"
            _check(load_token(env) == "from_env", "环境变量应优先于 .env")
            os.environ[TOKEN_ENV] = ""
            _check(load_token(env) is None, "环境变量设成空串应视为缺失，不回落 .env")
    finally:
        os.environ.pop(TOKEN_ENV, None)
        if saved is not None:
            os.environ[TOKEN_ENV] = saved
    print("[selftest 9 token] 环境变量优先、.env 解析（export / 引号 / 注释 / 重复键）、坏值拒收",
          file=sys.stderr)


def _with_ante(msgs, ante):
    """实测日志改写成「每个发牌座开局先交 ante」：快照栈 / bet 不变（= 每人多带 ante 入座），每手
    所有底池数字 + ante × 发牌人数，结算时把这份加给第一个收钱的座位（筹码守恒）；赢家净赢
    （winners[].amount = 结算栈 − 开局栈）随之调整。"""
    out, k = [], {}
    for m in msgs:
        m = dict(m)
        h = m.get("hand_id")
        if m["type"] == "table_state" and h not in k:
            k[h] = sum(1 for s in m["seats"] if s.get("in_hand"))
        extra = ante * k.get(h, 0)
        if m["type"] == "table_state":
            m["pot"] += extra
        elif m["type"] == "player_action":
            m["pot"] += extra
            m["pot_before"] += extra
        elif m["type"] == "hand_result":
            final = dict(m["final_stacks"])
            w = m["payouts"][0]["seat"]
            final[str(w)] += extra
            m["final_stacks"] = final
            m["winners"] = [dict(x, amount=x["amount"] - ante + (extra if x["seat"] == w else 0))
                            for x in m["winners"]]
        out.append(m)
    return out


def _selftest_ante():
    """--ante：底池按「人人先交 ante」核对、开局栈含 ante、请求 / hh 带 ante；比例对不上 blueprint、
    或说有 ante 而桌上没有 → 每手作废。"""
    msgs = _fixture_msgs()
    _, base_stub, base_rows = _run_msgs(msgs)
    buf = io.StringIO()
    session, stub, rows = _run_msgs(_with_ante(msgs, 2), ante=2, blueprint_ante=100, hh_log=buf)
    c = session.counters
    _check(not session.skip_reasons and c["hands_complete"] == 7 and c["result_notes"] == 0,
           f"带 ante 的会话应全部正常: {session.skip_reasons} {c}")
    _check(len(rows) == len(base_rows) and len(stub.requests) == len(base_stub.requests),
           "决策点应与无 ante 时一一对应")
    for q, bq, row, brow in zip(stub.requests, base_stub.requests, rows, base_rows):
        _check(q["ante"] == 2 and "ante" not in bq, "请求应带 ante（无 ante 时不带）")
        dealt = q.get("dealt_seats") or list(range(9))
        _check(all(q["stacks"][s] == bq["stacks"][s] + 2 for s in dealt), "开局栈应含 ante")
        _check(row["valid"] == brow["valid"] and row["to_call"] == brow["to_call"],
               "ante 不算本轮下注：合法区间 / 跟注额不变")
        _check(row["pot"] == brow["pot"] + 2 * len(dealt), "底池应含全部 ante")
    hh = [json.loads(line) for line in buf.getvalue().splitlines()]
    _check(len(hh) == 7 and all(r["ante"] == 2 for r in hh), "hh 行应带 ante")
    if os.environ.get("WOPO_REGEN_HH_ANTE"):
        with open(HH_ANTE_FIXTURE, "w") as f:
            f.write(buf.getvalue())
    with open(HH_ANTE_FIXTURE) as f:
        _check(hh == [json.loads(line) for line in f if line.strip()],
               f"ante hh 输出与 {HH_ANTE_FIXTURE} 不一致（改了格式用 WOPO_REGEN_HH_ANTE=1 重生成）")

    session, _, rows = _run_msgs(_with_ante(msgs, 2), ante=2, blueprint_ante=50)
    _check(session.skip_reasons == {"ante_mismatch": 8} and not rows,
           f"ante 比例与 blueprint 不符应每手作废: {session.skip_reasons}")
    session, _, rows = _run_msgs(msgs, ante=2, blueprint_ante=100)
    _check(session.skip_reasons == {"snapshot_pot": 8} and not rows,
           f"说有 ante 而桌上没有应作废: {session.skip_reasons}")
    print("[selftest 11 ante] 底池 / 开局栈按人人先交 ante 核对、请求与 hh 带 ante；比例不符或桌上没有"
          " → 作废", file=sys.stderr)


def _selftest_posts():
    """补盲：非盲注座开局 bet = 1BB 记为补盲座，底池 / 跟注额按 live 盲注核对，请求与 hh 带 posts。
    用 2026-10-28 实盘 4 手（修前全部 blinds[N]:bet=100,want=0 作废）。补的不是 1BB、补盲就全下 → 作废。"""
    with open(POSTS_FIXTURE) as f:
        msgs = [json.loads(line) for line in f if line.strip()]
    buf, play = io.StringIO(), io.StringIO()
    session, stub, rows = _run_msgs(msgs, ante=100, blueprint_ante=100, n_seats=8, hh_log=buf,
                                    play_log=play)
    c = session.counters
    _check(not session.skip_reasons and c["hands_complete"] == 4 and c["result_notes"] == 0,
           f"补盲手应全部正常: {session.skip_reasons} {c}")
    hh = [json.loads(line) for line in buf.getvalue().splitlines()]
    want = {"122": [6], "123": [2], "127": [7], "132": [4]}
    _check({r["hand_id"][-3:]: r.get("posts") for r in hh} == want, f"hh posts: {hh}")
    _check(stub.requests and all(q.get("posts") == want[q_hand[-3:]]
                                 for q, q_hand in zip(stub.requests, [r["hand_id"] for r in rows])),
           "请求应带本手 posts")
    _check("补盲" in play.getvalue(), "play.log 座位行应标出补盲座")
    _, base_stub, _ = _run_msgs(_fixture_msgs())
    _check(all("posts" not in q for q in base_stub.requests), "没人补盲时请求不带 posts")
    if os.environ.get("WOPO_REGEN_HH_POSTS"):
        with open(HH_POSTS_FIXTURE, "w") as f:
            f.write(buf.getvalue())
    with open(HH_POSTS_FIXTURE) as f:
        _check(hh == [json.loads(line) for line in f if line.strip()],
               f"补盲 hh 输出与 {HH_POSTS_FIXTURE} 不一致（改了格式用 WOPO_REGEN_HH_POSTS=1 重生成）")

    def tamper(bet=None, stack=None):
        out, done = [], False
        for m in msgs:
            m = json.loads(json.dumps(m))
            if (not done and m.get("type") == "table_state"
                    and m.get("hand_id") == "ante-table:hand:000132"):
                seat4 = m["seats"][4]
                if bet is not None:
                    m["pot"] += bet - seat4["bet"]
                    seat4["bet"] = bet
                if stack is not None:
                    seat4["stack"] = stack
                done = True
            out.append(m)
        return out
    session, _, _ = _run_msgs(tamper(bet=50), ante=100, blueprint_ante=100, n_seats=8)
    _check(session.skip_reasons == {"blinds[4]": 1}, f"补的不是 1BB 应作废: {session.skip_reasons}")
    session, _, _ = _run_msgs(tamper(stack=0), ante=100, blueprint_ante=100, n_seats=8)
    _check(session.skip_reasons == {"post_all_in": 1}, f"补盲就全下应作废: {session.skip_reasons}")
    print("[selftest 12 posts] 补盲按 live 1BB 核对（实盘 4 手不再作废）、请求与 hh 带 posts；"
          "补的不是 1BB / 补盲全下 → 作废", file=sys.stderr)


def run_selftest():
    _selftest_full_replay()
    _selftest_mid_hand_join()
    _selftest_tamper()
    _selftest_stale()
    _selftest_engine_rules()
    _selftest_legalize()
    _selftest_exploit_4bet()
    _selftest_hh_log()
    _selftest_token()
    _selftest_table_size()
    _selftest_ante()
    _selftest_posts()
    print("OK: wopo selftest 12 组场景全部通过。", file=sys.stderr)


# ===========================================================================
def main():
    p = argparse.ArgumentParser(description="wopo 牌桌 driver（只收不发，建议写行动文件）+ Rust advisor")
    p.add_argument("--selftest", action="store_true",
                   help="离线自测（stub advisor，不需要 blueprint / 其余参数）")
    if "--selftest" in sys.argv[1:]:
        p.parse_args()
        run_selftest()
        return
    add_advisor_args(p, default_seats=9,
                     debug_help="打印牌局中间数据到 stderr（手起/发牌/动作/决策/结果）+ 透传 advisor "
                                "--debug-log；纯展示")
    src = p.add_mutually_exclusive_group()
    src.add_argument("--url", default=DEFAULT_URL, help=f"wopo WS 地址（默认 {DEFAULT_URL}）")
    src.add_argument("--replay", help="回放文件（原始消息逐行，或 --ws-log 格式）")
    p.add_argument("--action-file", required=True,
                   help="行动文件（外部程序读）：每个决策点追加一行 JSON")
    p.add_argument("--action-log", default="wopo_actions_log.jsonl",
                   help="每决策落 {req,resp,decide_ms,stale} JSONL（含过期的）；空串关闭")
    p.add_argument("--ws-log", default=None, help="原始 WS 消息逐条落 JSONL（append）；缺省关")
    p.add_argument("--hh-log", default=None,
                   help="每手打完且没作废的牌局落 JSONL（append；opponent_analysis --platform wopo 读）；缺省关")
    p.add_argument("--play-log", default=None,
                   help="牌桌实况（人看，append）：手牌 / 公共牌 / 玩家行动 / hero 决策（建议、来源、耗时）/ "
                        "实际行动 / 结算 / 作废；诊断信息仍走 stderr。缺省关")
    p.add_argument("--num-hands", type=int, default=0, help="打完这么多手后退出；0 = 不限")
    p.add_argument("--ante", type=int, default=0,
                   help="桌面每座 ante（桌面筹码，默认 0）：开局人人先交，计入底池、不算本轮下注")
    p.add_argument("--blueprint-ante", type=int, default=0,
                   help="blueprint 的每座 ante（solver 筹码，BB = 100，= 训练时 train_cfr --ante；默认 0）。"
                        "每手要求 --ante × 100 == 本值 × 大盲")
    add_fourbet_args(p)
    args = p.parse_args()
    if args.ante < 0 or not 0 <= args.blueprint_ante <= 100:
        p.error("--ante 须 ≥ 0，--blueprint-ante 须在 [0, 100]")
    validate_advisor_args(args)
    validate_fourbet_args(p, args)
    token = None
    if not args.replay:
        token = load_token()
        if token is None:
            p.error(f"连 WS 需要 token：设环境变量 {TOKEN_ENV} 或写进 {ENV_FILE}（非空且不含空白）")

    ante_args = ["--ante", str(args.blueprint_ante)] if args.blueprint_ante else []
    advisor = start_advisor(args, advisor_extra_args(args) + fourbet_extra_args(args) + ante_args)
    # 回放用同步 worker（结果确定、不会因为读文件太快把决策判成过期）；联机用后台线程。
    worker = DecisionWorker(advisor, sync=args.replay is not None)
    action_file = ActionFile(args.action_file)
    action_log = open(args.action_log, "a") if args.action_log else None
    hh_log = open(args.hh_log, "a", encoding="utf-8") if args.hh_log else None
    ws_log = WsLog(args.ws_log) if args.ws_log else None
    play_log = open(args.play_log, "a", encoding="utf-8") if args.play_log else None
    session = WopoSession(worker, action_file, n_seats=args.seats, hero_seat=HERO_SEAT,
                          num_hands=args.num_hands, action_log=action_log,
                          prewarm=args.search_prewarm, exploit=exploit_enabled(args),
                          debug=args.debug_log, exploit_4bet=fourbet_enabled(args), hh_log=hh_log,
                          play_log=play_log, ante=args.ante, blueprint_ante=args.blueprint_ante)
    ok = True
    try:
        if args.replay:
            run_replay(session, args.replay)
        else:
            ok = run_ws(session, args.url, token, ws_log=ws_log)
    finally:
        worker.close(timeout=30)
        advisor.close()
        session.report()
        action_file.close()
        if action_log:
            action_log.close()
        if hh_log:
            hh_log.close()
        if ws_log:
            ws_log.close()
        if play_log:
            play_log.close()
    if not ok:
        sys.exit(1)


if __name__ == "__main__":
    main()

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
  - 画像暖启动（--warm-hh，开了 --exploit / --exploit-4bet 时默认开）：启动后、第一手之前，把历史 hh 里
    每手按实盘同一格式（table.observe_from_hh）当 observe 发给 advisor，进程内画像不再从空表开始。缺省
    来源 = --hh-log 所在 run 目录的上一级下所有 */wopo_hh.jsonl（同级各轮 + 本轮已写的，重启能接上）；
    给 glob（可重复）覆盖，off 关闭。按 hand_id 去重、按 ts 排序。
  - --play-log：给人看的牌桌实况（手牌 / 公共牌 / 玩家行动 / hero 决策与实际行动 / 结算 / 作废），
    诊断信息（advisor 启动、连接、过期、未知消息）仍走 stderr。
  - --play-events：同一份实况的结构化事件流（JSONL，play log 每行对应一条事件，带同一行文字 text），
    给 opponent_analysis live --play-events 在网页上实时显示「行动指南」。缺省：给了 --play-log 时写到
    同目录的 wopo_play_events.jsonl；--play-events off 关闭。只记录，不影响决策 / 行动文件 / hh /
    action log。事件格式见 opponent_analysis/README.md「行动指南」。
  - --ante（桌面筹码，默认 0）：开局每个发牌座先交 ante 再发盲注，牌桌层按此核对底池 / 栈（快照 bet
    只含盲注；对不上 → 作废）。--blueprint-ante（solver 筹码，BB = 100，= train_cfr --ante）透传给
    advisor 加载对应 blueprint；每手要求 ante × 100 == blueprint-ante × 大盲，否则这手作废
    （ante_mismatch）。短桌空座在实时搜索里不交 ante（树里是按满桌人人交训的，翻前查表照用）。
  - --stack-bb（默认 100）= blueprint 训练时的起始码深（train_cfr --stack-bb），透传给 advisor；
    advisor ready 行回报的码深须一致，否则启动报错。
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
  wopo_play.py ... --play-log $D/play.log  同时写 $D/wopo_play_events.jsonl（行动指南事件流）；网页看：
      python3 -m opponent_analysis live --db <库> --platform wopo --follow $D/wopo_hh.jsonl \\
          --play-events $D/wopo_play_events.jsonl --hero hero      （http://127.0.0.1:8780/）
  wopo_play.py --selftest                 离线自测（stub advisor，不需要 blueprint）
"""

import argparse
import io
import json
import os
import re
import shlex
import sys
import tempfile
import threading
import time

from poker_live.advisor_client import (add_advisor_args, add_fourbet_args, advisor_extra_args,
                                       exploit_enabled, fourbet_enabled, fourbet_extra_args,
                                       start_advisor, validate_advisor_args, validate_fourbet_args)
from poker_live.decision import DecisionWorker
from poker_live.table import HandEngine, HandInvalid, observe_from_hh
from poker_live.wopo import _STALE_CN, _STREET_CN, ActionFile, WopoSession, _fmt_act, fmt_probs, legalize
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
# FIXTURE 回放（exploit + prewarm）的 --play-events 输出，ts / decide_ms 规范化后（_selftest_play_events
# 生成并核对；opponent_analysis 的行动指南测试读它）。
PLAY_EVENTS_FIXTURE = os.path.join(os.path.dirname(HH_FIXTURE), "wopo_play_events_2026_10_16.jsonl")


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
              n_seats=9, send_hand_id=False, play_events=None):
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
                              blueprint_ante=blueprint_ante, send_hand_id=send_hand_id,
                              play_events=play_events)
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


def _selftest_post_rewrite():
    """补盲不进 advisor：补盲者翻前没人加注时的过牌改发 call；大盲的过牌、补盲者翻后的过牌和其余动作原样。"""
    e = HandEngine("h", 0, 1, 2, HERO_SEAT, 5)
    seats = {s: {"seat": s, "stack": st, "bet": b, "in_hand": True, "folded": False}
             for s, st, b in [(0, 100, 0), (1, 99, 1), (2, 98, 2), (3, 98, 2), (4, 100, 0)]}
    e.apply_hole(["Ah", "Kd"])
    e.apply_snapshot({"street": "preflop", "board": [], "pot": 5, "actor": 3, "seats": seats,
                      "n_seats": 5})
    _check(e.posts == [3], f"座 3 补盲: {e.posts}")

    def act(seat, action, delta, to=None):
        st, pot = e.stack[seat], e.pot
        e.apply_action({"seat": seat, "action": action, "street": e.street, "delta": delta,
                        "to": to, "stack_before": st, "stack_after": st - delta,
                        "pot_before": pot, "pot_after": pot + delta})

    act(3, "check", 0)                         # 补盲者没人加注时过牌
    act(4, "fold", 0)
    _check(e.advisor_actions() == [{"seat": 3, "action": "call"}, {"seat": 4, "action": "fold"}],
           f"补盲过牌应改发 call: {e.advisor_actions()}")
    _check(e.actions[0] == {"seat": 3, "action": "check"}, f"牌桌层历史不改: {e.actions[0]}")
    act(0, "call", 2)
    act(1, "call", 1)
    act(2, "check", 0)                         # 大盲过牌：不是补盲，原样
    e.apply_board("flop", ["2c", "7d", "9h"])
    act(1, "check", 0)
    act(2, "check", 0)
    act(3, "check", 0)                         # 补盲者翻后过牌：原样
    got = e.advisor_actions()
    _check([a["action"] for a in got] == ["call", "fold", "call", "call", "check",
                                          "check", "check", "check"], f"{got}")
    print("[selftest 5b 补盲改写] 补盲者翻前没人加注时的过牌改发 call，其余原样", file=sys.stderr)


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
    _check(all(isinstance(rec.get("button_seat"), int) and rec.get("dealt_seats")
               and isinstance(rec.get("posts"), list)
               for k, rec in stub.side if k == "observe"),
           "observe 要带 button_seat / dealt_seats / posts（画像的未开池统计用）")
    _, stub, _ = _run_msgs(msgs)
    _check(not stub.side and all("names" not in q for q in stub.requests), "剥削全关时不带名字、不发 observe")
    _check(all("hand_id" not in q for q in stub.requests), "没开 --range-log 时请求不带 hand_id")
    _, stub, rows = _run_msgs(msgs, send_hand_id=True)
    _check(stub.requests and all(q.get("hand_id") for q in stub.requests), "开 --range-log 时请求带 hand_id")
    _check({q["hand_id"] for q in stub.requests} >= {r["hand_id"] for r in rows},
           "请求的 hand_id 与行动文件一致")
    print("[selftest 7 PM-17] 请求带名字、observe 带投入金额并与服务器逐条一致", file=sys.stderr)


def _selftest_warm_hh():
    """画像暖启动：从 hh 重建的 observe 与这些手实盘当时发给 advisor 的 observe 逐条相同（含补盲手、带金额的
    PM-17 格式）；多文件按 ts 排序、hand_id 去重、坏行跳过；缺省来源 / off / 显式 glob。"""
    norm = lambda o: json.loads(json.dumps(o, ensure_ascii=False))  # 实盘 names 的 int 键序列化后是字符串
    cases = [(_fixture_msgs(), {}), ]
    with open(POSTS_FIXTURE) as f:
        cases.append(([json.loads(line) for line in f if line.strip()],
                      {"ante": 100, "blueprint_ante": 100, "n_seats": 8}))
    with tempfile.TemporaryDirectory() as d:
        files = []
        for k, (msgs, kw) in enumerate(cases):
            for amounts in (False, True):
                buf = io.StringIO()
                _, stub, _ = _run_msgs(msgs, hh_log=buf, exploit=not amounts, exploit_4bet=amounts, **kw)
                live = [norm(rec) for kind, rec in stub.side if kind == "observe"]
                path = os.path.join(d, f"r{k}", WARM_HH_NAME)
                os.makedirs(os.path.dirname(path), exist_ok=True)
                with open(path, "w", encoding="utf-8") as f:
                    f.write(buf.getvalue())
                got, st = load_warm_observes([path], amounts=amounts)
                _check(live and got == live and st == {"files": 1, "hands": len(live), "bad": 0, "dup": 0},
                       f"第 {k} 组 amounts={amounts}：hh 重建的 observe 与实盘不同 {st}")
                if k == 1:
                    _check(any(o["posts"] for o in got), "补盲手的 observe 要带 posts")
            files.append(path)
        # 两个文件一起：按 ts 排序（补盲那组是 10-28 实盘，fixture 是 10-16），同一文件再给一次 → 全部算重复；坏行跳过。
        with open(files[0], "a", encoding="utf-8") as f:
            f.write("not json\n" + json.dumps({"wopo_hh": 1, "hand_id": "x"}) + "\n")
        got, st = load_warm_observes([os.path.join(d, "*", WARM_HH_NAME), files[1]])
        n0 = len(load_warm_observes([files[0]])[0])
        _check(st == {"files": 2, "hands": len(got), "bad": 2, "dup": 0} and len(got) == n0 + 4,
               f"多文件统计 {st}")
        ts = [json.loads(line).get("ts") for f in files for line in open(f, encoding="utf-8")
              if line.startswith("{") and "result" in line]
        _check(got[:n0] == load_warm_observes([files[0]])[0] and min(ts[:n0]) < min(ts[n0:]),
               "按 ts 排序：fixture（10-16）在补盲那组（10-28）之前")
        _, st = load_warm_observes([files[0], files[0], os.path.join(d, "r0", "*.jsonl")])
        _check(st["dup"] == 0 and st["files"] == 1, f"同一文件多次匹配只读一次 {st}")
        dup = os.path.join(d, "copy", WARM_HH_NAME)
        os.makedirs(os.path.dirname(dup))
        with open(files[1], encoding="utf-8") as src, open(dup, "w", encoding="utf-8") as dst:
            dst.write(src.read())
        _, st = load_warm_observes([files[1], dup])
        _check(st["hands"] == 4 and st["dup"] == 4, f"不同文件里同一手按 hand_id 去重 {st}")
    run = os.path.join("x", "runs", "r1", "wopo_hh.jsonl")
    _check(resolve_warm_hh(None, run) == [os.path.join(os.path.abspath(os.path.join("x", "runs")), "*", WARM_HH_NAME)]
           and resolve_warm_hh(None, None) == [] and resolve_warm_hh(["off"], run) == []
           and resolve_warm_hh(["a/*.jsonl", "b.jsonl"], run) == ["a/*.jsonl", "b.jsonl"], "--warm-hh 缺省规则")
    print("[selftest 14 画像暖启动] hh 重建的 observe 与实盘逐条相同；多文件排序 / 去重 / 坏行；缺省来源", file=sys.stderr)


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


_NOT_CHIPS = {"seat", "dealer_seat", "actor_seat", "hand_seq", "table_seq"}


def _scale_chips(o, k, key=None):
    """消息里的筹码数字全部 × k（座位号 / 序号不动）。"""
    if isinstance(o, dict):
        return {kk: _scale_chips(v, k, kk) for kk, v in o.items()}
    if isinstance(o, list):
        return [_scale_chips(v, k, key) for v in o]
    if isinstance(o, int) and not isinstance(o, bool) and key not in _NOT_CHIPS:
        return o * k
    return o


def _selftest_chip_div():
    """大盲不整除 100 的桌（100/200）：发给 advisor 的筹码 ÷ chip_div，建议的加注额 × chip_div 还原。
    实测会话（1/2）筹码 ×100 改成 100/200 → chip_div = 2：每个请求 = 原请求筹码 ×50（advisor 侧 scale
    从 50 变 1，看到的 solver 筹码不变）；整除的桌 chip_div = 1、请求不变。另核对单数筹码的全下。"""
    from poker_live.wopo import chip_div, downscale_request, upscale_response
    _check(chip_div(1, 2, 0) == 1 and chip_div(20, 50, 50) == 1 and chip_div(50, 100, 100) == 1
           and chip_div(100, 200, 200) == 2 and chip_div(150, 300, 300) == 3
           and chip_div(100, 300, 300) == 1
           and chip_div(75, 150, 0) == 3 and chip_div(25, 200, 0) == 1, "chip_div 取值")

    def reply(req):
        v = req["valid"]
        if v.get("can_raise") and v["max_raise"] - v["min_raise"] >= 2:
            return {"action": "raise", "amount": (v["min_raise"] + v["max_raise"]) // 2,
                    "source": "stub", "probs": [["raise", 1.0]], "chosen": "raise"}
        return None

    msgs = _fixture_msgs()
    _, base, base_rows = _run_msgs(msgs, reply=reply)
    session, stub, rows = _run_msgs(_scale_chips(msgs, 100), reply=reply)
    _check(not session.skip_reasons and session.counters["hands_complete"] == 7,
           f"100/200 会话应全部正常: {session.skip_reasons}")
    _check(len(stub.requests) == len(base.requests) and len(rows) == len(base_rows), "决策点应一一对应")

    def x50(q):
        q = _scale_chips(q, 50)  # 请求里 seat / button_seat 等不是筹码：逐键还原
        for k in ("button_seat", "my_seat", "num_seats"):
            q[k] //= 50
        q["dealt_seats"] = [s // 50 for s in q["dealt_seats"]] if "dealt_seats" in q else None
        if q["dealt_seats"] is None:
            del q["dealt_seats"]
        return q

    for q, bq in zip(stub.requests, base.requests):
        _check(q == x50(bq), f"请求应 = 原请求筹码 ×50: {q} vs {bq}")
    raised = 0
    _check(len(rows) == len(stub.requests), "同步回放：每个决策都写进行动文件")
    for r, br, q in zip(rows, base_rows, stub.requests):
        _check(r["action"] == br["action"], "动作应相同")
        if r["action"] == "raise":
            raised += 1
            sv = q["valid"]
            # stub 在缩小后的区间里取中点 a'；前面各街投入是偶数（×100 改写）→ 桌面额 = a' × 2。
            _check(r["amount"] == (sv["min_raise"] + sv["max_raise"]) // 2 * 2
                   and r["valid"]["min_raise"] <= r["amount"] <= r["valid"]["max_raise"],
                   f"加注额应还原成桌面筹码: {r['amount']} vs {sv}")
    _check(raised > 0, "应有加注建议")

    # 单数筹码：翻前加注到 601（单数），翻牌 hero 全下 9199 = 10000 − ante 200 − 601。
    req = {"small_blind": 100, "big_blind": 200, "ante": 200,
           "actions": [{"seat": 1, "action": "raise", "to": 601}, {"seat": 0, "action": "call"},
                       {"seat": 1, "action": "raise", "to": 333}],
           "valid": {"can_check": False, "can_call": True, "can_raise": True,
                     "min_raise": 666, "max_raise": 9199},
           "stacks": [10000, 20001]}
    sreq, prior = downscale_request(req, 2, ["preflop", "preflop", "flop"], "flop")
    pre_to = sreq["actions"][0]["to"]
    _check(sreq["small_blind"] == 50 and sreq["big_blind"] == 100 and sreq["ante"] == 100
           and sreq["stacks"] == [5000, 10001] and prior == 601, f"缩小后的请求 {sreq}")
    _check(sreq["ante"] + pre_to + sreq["valid"]["max_raise"] == sreq["stacks"][0],
           f"全下应恰好用完缩小后的栈: {sreq}")
    v, sv = req["valid"], sreq["valid"]
    up = lambda a: upscale_response({"action": "raise", "amount": a}, 2, prior, v, sv)["amount"]
    _check(up(sv["max_raise"]) == 9199 and up(sv["max_raise"] + 5) == 9199
           and up(sv["min_raise"]) == 666 and up(1) == 666, "上下限还原")
    mid = up(3000)
    _check(_rdiv_check(601 + mid, 2) == pre_to + 3000, f"中间额按累计投入还原: {mid}")
    _check(upscale_response({"action": "call"}, 2, prior, v, sv) == {"action": "call"}, "call 不动")
    print("[selftest 15 筹码缩放] 100/200 桌请求 = 1/2 桌 ×50、加注额 ×2 还原；单数筹码全下恰好用完栈",
          file=sys.stderr)


def _rdiv_check(x, d):
    return (x + d // 2) // d


def _selftest_posts():
    """补盲：非盲注座开局 bet = 1BB 记为补盲座，底池 / 跟注额按 live 盲注核对，hh 带 posts；advisor 请求
    不带补盲，补盲者翻前没人加注时的过牌改发 call（advisor 没有补盲逻辑）。
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
    # 补盲不进 advisor 请求：补盲者翻前没人加注时的过牌改发 call，其余动作原样（HandEngine.advisor_actions）。
    by_hand = {r["hand_id"]: r for r in hh}
    for q, q_hand in zip(stub.requests, [r["hand_id"] for r in rows]):
        h = by_hand[q_hand]
        want_acts = []
        for a in h["actions"][:len(q["actions"])]:
            post_check = a["seat"] in h["posts"] and a["action"] == "check" and a["street"] == "preflop"
            want_acts.append({"seat": a["seat"], "action": "call" if post_check else a["action"],
                              **({"to": a["to"]} if "to" in a else {})})
        _check("posts" not in q and q["actions"] == want_acts,
               f"{q_hand} 请求应不带 posts、补盲过牌改发 call: {q['actions']} vs {want_acts}")
    _check(stub.requests, "补盲手应有 hero 决策")  # 补盲过牌的改写由 _selftest_post_rewrite 覆盖。
    _check("补盲" in play.getvalue(), "play.log 座位行应标出补盲座")
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
    print("[selftest 12 posts] 补盲按 live 1BB 核对（实盘 4 手不再作废）、hh 带 posts、请求不带补盲"
          "（补盲过牌改发 call）；"
          "补的不是 1BB / 补盲全下 → 作废", file=sys.stderr)


class _EventSink:
    """--play-events 的内存版：每条事件过一遍 JSON（保证可序列化）后留下。"""

    def __init__(self):
        self.events = []

    def write(self, rec):
        self.events.append(json.loads(json.dumps(rec, ensure_ascii=False)))


# 每种事件除公共字段（ts / type / hand_id / table_id / text）外的字段（opponent_analysis/README.md 有说明）。
_EVENT_COMMON = {"ts", "type", "hand_id", "table_id", "text"}
_ACTION_KEYS = {"seat", "name", "street", "turn", "action", "to", "delta", "to_call_before",
                "stack_before", "stack_after", "pot_after"}
EVENT_KEYS = {
    "hand_start": {"button", "sb", "bb", "ante", "hero_seat", "n_seats"},
    "dead": {"reason", "tag"},
    "hole": {"cards"},
    "seats": {"seats", "button", "sb_seat", "bb_seat", "posts", "pot"},
    "board": {"street", "cards", "board", "pot"},
    "action": _ACTION_KEYS,
    "hero_action": _ACTION_KEYS | {"advice", "match"},
    "hero_turn": {"turn", "street", "pot", "to_call", "valid", "hole", "board", "table_seq"},
    "decision": {"turn", "street", "pot", "to_call", "valid", "action", "amount", "all_in", "advice",
                 "probs", "chosen", "source", "decide_ms", "stale", "stale_kind", "written",
                 "rejected"},
    "result": {"stacks", "pot", "board", "winners", "shown", "notes", "server_ts"},
}


def _render_event(e):
    """只用事件的结构化字段（不看 text）还原 play log 那一行：核对事件足够让网页不解析文字。"""
    t = e["type"]

    def who(seat, name):
        return f"seat{seat} {name}" if name else f"seat{seat}"

    if t == "hand_start":
        return (f"──── {e['hand_id']} 庄位 seat{e['button']} 盲注 {e['sb']}/{e['bb']}"
                + (f" ante {e['ante']}" if e["ante"] else ""))
    if t == "dead":
        _check(e["tag"] == e["reason"].split(":", 1)[0], f"dead tag {e}")
        return f"本手作废：{e['reason']}（之后不出建议）"
    if t == "hole":
        return "底牌 " + " ".join(e["cards"])
    if t == "seats":
        return "座位 " + " | ".join(
            f"{who(x['seat'], x['name'])} {x['start']}" + (f"({','.join(x['roles'])})" if x["roles"] else "")
            for x in e["seats"])
    if t == "board":
        _check(e["board"][-len(e["cards"]):] == e["cards"], f"board 新牌 {e}")
        return f"{_STREET_CN[e['street']]} {' '.join(e['board'])}"
    if t in ("action", "hero_action"):
        _check(e["stack_after"] == e["stack_before"] - e["delta"], f"动作栈 {e}")
        line = f"{who(e['seat'], e['name'])} " + _fmt_act(e["action"], e["to"], e["delta"],
                                                          e["to_call_before"])
        if t == "action":
            return line
        if e["advice"] is None:
            _check(e["match"] is None, f"无建议时 match 应为 null: {e}")
            return f"{line}  ← hero 实际行动（无建议）"
        return (f"{line}  ← hero 实际行动（{'同' if e['match'] == 'same' else '不同于'}建议 "
                f"{e['advice']['text']}）")
    if t == "hero_turn":
        return f">> 轮到 hero（{_STREET_CN[e['street']]}，底池 {e['pot']}，跟 {e['to_call']}）"
    if t == "decision":
        advice = _fmt_act(e["action"], e["amount"], e["to_call"], e["to_call"])
        if e["action"] == "raise" and e["amount"] == e["valid"].get("max_raise"):
            advice += "（全下）"
        _check(advice == e["advice"], f"建议文字 {advice!r} ≠ {e['advice']!r}")
        _check(e["written"] == (e["stale"] is None)
               and e["stale_kind"] == (e["stale"].split(":", 1)[0] if e["stale"] else None),
               f"决策过期字段 {e}")
        stale = e["stale"]
        return (f">> 决策  来源 {e['source']}  {e['decide_ms']}ms"
                + (f"  已过期（{_STALE_CN.get(e['stale_kind'], stale)}），未写入行动文件" if stale else "")
                + (f"  {fmt_probs(e['probs'])}" if e["probs"] else "")
                + f"  建议 {advice}")
    if t == "result":
        net = [f"{who(x['seat'], x['name'])} {x['net']:+d}" for x in e["stacks"]
               if x["final"] is not None and x["final"] != x["start"]]
        return "结算 " + (" | ".join(net) or "无输赢")
    raise RuntimeError(f"[selftest] 未知事件类型 {t}")


def _check_events(play_text, events, session=None, rows=None):
    """事件流和 play log 一一对应（同一时刻、同一行文字、类型与行首一致），字段齐全，且只用结构化
    字段就能还原每一行；给了 session / rows 时再和计数器、行动文件对账。"""
    lines = play_text.splitlines()
    _check(len(lines) == len(events), f"事件 {len(events)} 条 ≠ play log {len(lines)} 行")
    prefix = {"hand_start": "────", "hole": "底牌 ", "seats": "座位 ", "hero_turn": ">> 轮到 hero",
              "decision": ">> 决策  来源 ", "result": "结算 ", "dead": "本手作废："}
    turns = set()
    for ln, e in zip(lines, events):
        clock, text = ln.split(" ", 1)
        t = e["type"]
        _check(t in EVENT_KEYS and set(e) == _EVENT_COMMON | EVENT_KEYS[t],
               f"{t} 事件字段 {sorted(e)}")
        _check(e["text"] == text and time.strftime("%H:%M:%S", time.localtime(e["ts"])) == clock,
               f"事件与 play log 行不对应: {e} / {ln}")
        _check(_render_event(e) == text, f"结构化字段还原不出这一行:\n  {_render_event(e)}\n  {text}")
        if t in prefix:
            _check(text.startswith(prefix[t]), f"{t} ↔ {text}")
        elif t == "board":
            _check(text.startswith(("翻牌 ", "转牌 ", "河牌 ")), f"board ↔ {text}")
        else:
            _check(text.startswith("seat") and (("← hero 实际行动" in text) == (t == "hero_action")),
                   f"{t} ↔ {text}")
        if t == "hero_turn":
            turns.add((e["hand_id"], e["turn"]))
        if t == "decision":
            _check((e["hand_id"], e["turn"]) in turns, f"决策之前没有对应的 hero_turn: {e}")
    if session is not None:
        c = session.counters
        n = lambda t, pred=lambda e: True: sum(1 for e in events if e["type"] == t and pred(e))
        _check(n("decision") == c["decisions"] and n("hero_turn") == c["turns"]
               and n("decision", lambda e: e["written"]) == c["written"]
               and n("decision", lambda e: not e["written"]) == c["stale"]
               and n("hero_action", lambda e: e["match"] == "same") == c["hero_same"]
               and n("hero_action", lambda e: e["match"] == "diff") == c["hero_diff"]
               and n("hero_action", lambda e: e["match"] is None) == c["hero_unadvised"]
               and n("result") == c["hands_complete"]
               and n("dead") == sum(session.skip_reasons.values()), f"事件计数与计数器 {c}")
    if rows is not None:
        written = [e for e in events if e["type"] == "decision" and e["written"]]
        _check([(e["hand_id"], e["action"], e["amount"], e["probs"], e["source"], e["decide_ms"],
                 e["pot"], e["to_call"], e["valid"]) for e in written]
               == [(r["hand_id"], r["action"], r["amount"], r["probs"], r["source"], r["decide_ms"],
                    r["pot"], r["to_call"], r["valid"]) for r in rows], "已写决策事件 ≠ 行动文件")


def _norm_events(events):
    """固定 ts / decide_ms（stub advisor 的耗时可能是 0 或 1ms），让输出能和 fixture 逐条比。"""
    out = []
    for i, e in enumerate(events):
        e = dict(e, ts=1792130000.0 + 2 * i)
        if e["type"] == "decision":
            e["decide_ms"] = 0
            e["text"] = re.sub(r"  \d+ms", "  0ms", e["text"], count=1)
        out.append(e)
    return out


def _selftest_play_events():
    """--play-events：和 play log 逐行对应、字段齐、能还原文字；开不开事件流，行动文件 / hh / play log /
    advisor 请求逐字节相同；作废 / 过期 / 无建议 / 补盲 / ante 都有对应事件；off 不写文件。"""
    msgs = _fixture_msgs()
    play, hh, sink = io.StringIO(), io.StringIO(), _EventSink()
    session, stub, rows = _run_msgs(msgs, exploit=True, prewarm=True, play_log=play, hh_log=hh,
                                    play_events=sink)
    ev = sink.events
    _check_events(play.getvalue(), ev, session, rows)
    _check([e["type"] for e in ev[:4]] == ["hand_start", "hole", "seats", "action"],
           f"一手开头的事件顺序 {[e['type'] for e in ev[:4]]}")
    # 不开事件流：其余输出完全一样（play log 只比文字，时间戳会差）。
    play0, hh0 = io.StringIO(), io.StringIO()
    _, stub0, rows0 = _run_msgs(msgs, exploit=True, prewarm=True, play_log=play0, hh_log=hh0)
    strip = lambda txt: [ln.split(" ", 1)[1] for ln in txt.splitlines()]
    nots = lambda rs: [{k: v for k, v in r.items() if k not in ("ts", "decide_ms")} for r in rs]
    _check(nots(rows) == nots(rows0) and hh.getvalue() == hh0.getvalue()
           and strip(play.getvalue()) == strip(play0.getvalue())
           and stub.requests == stub0.requests and stub.side == stub0.side,
           "开事件流不该改变行动文件 / hh / play log / advisor 请求")
    norm = _norm_events(ev)
    if os.environ.get("WOPO_REGEN_PLAY_EVENTS"):
        with open(PLAY_EVENTS_FIXTURE, "w", encoding="utf-8") as f:
            f.writelines(json.dumps(e, ensure_ascii=False) + "\n" for e in norm)
    with open(PLAY_EVENTS_FIXTURE, encoding="utf-8") as f:
        _check(norm == [json.loads(line) for line in f if line.strip()],
               f"事件流与 {PLAY_EVENTS_FIXTURE} 不一致（改了格式用 WOPO_REGEN_PLAY_EVENTS=1 重生成）")

    # 作废：断线 → dead 事件（之后这手不再有决策事件）。
    i = next(i for i, m in enumerate(msgs) if m.get("hand_id") == "local-table:hand:000009"
             and m["type"] == "community_cards")
    play, sink = io.StringIO(), _EventSink()
    session, _, rows = _run_msgs(msgs[:i] + [("disconnect",)] + msgs[i:], play_log=play,
                                 play_events=sink)
    _check_events(play.getvalue(), sink.events, session, rows)
    dead = [e for e in sink.events if e["type"] == "dead"]
    _check([(e["hand_id"], e["reason"]) for e in dead] == [("local-table:hand:000009", "disconnected")],
           f"断线作废事件 {dead}")
    k = sink.events.index(dead[0])
    _check(not any(e["hand_id"] == dead[0]["hand_id"] and e["type"] in ("decision", "hero_turn")
                   for e in sink.events[k:]), "作废后不该再有这手的决策事件")

    # 过期 + 无建议（同 selftest 4：advisor 卡到 hero 第一次实际行动之后）。
    gate = threading.Event()
    first_hero_act = next(i for i, m in enumerate(msgs)
                          if m["type"] == "player_action" and m["seat"] == HERO_SEAT)

    def hook(i, session):
        if i == first_hero_act:
            gate.set()
        if i >= first_hero_act:
            deadline = time.time() + 10
            while session.counters["decisions"] < session.counters["turns"] and time.time() < deadline:
                time.sleep(0.001)

    play, sink = io.StringIO(), _EventSink()
    session, _, rows = _run_msgs(msgs, gate=gate, sync=False, hook=hook, play_log=play,
                                 play_events=sink)
    _check_events(play.getvalue(), sink.events, session, rows)
    stale = [e for e in sink.events if e["type"] == "decision" and not e["written"]]
    unadv = [e for e in sink.events if e["type"] == "hero_action" and e["advice"] is None]
    _check(len(stale) == 1 and stale[0]["stale_kind"] == "hero_acted" and len(unadv) == 1
           and (unadv[0]["hand_id"], unadv[0]["turn"]) == (stale[0]["hand_id"], stale[0]["turn"])
           and sink.events.index(unadv[0]) < sink.events.index(stale[0]),
           f"过期决策 / 无建议事件 {stale} {unadv}")

    # 补盲 + ante：座位事件标出补盲座，hand_start 带 ante。
    with open(POSTS_FIXTURE) as f:
        pmsgs = [json.loads(line) for line in f if line.strip()]
    play, sink = io.StringIO(), _EventSink()
    session, _, rows = _run_msgs(pmsgs, ante=100, blueprint_ante=100, n_seats=8, play_log=play,
                                 play_events=sink)
    _check_events(play.getvalue(), sink.events, session, rows)
    seats = [e for e in sink.events if e["type"] == "seats"]
    _check({e["hand_id"][-3:]: e["posts"] for e in seats} == {"122": [6], "123": [2], "127": [7], "132": [4]}
           and all("补盲" in x["roles"] for e in seats for x in e["seats"] if x["seat"] in e["posts"])
           and all(e["ante"] == 100 for e in sink.events if e["type"] == "hand_start"),
           "补盲 / ante 事件")
    _check(all(x["start"] == x["stack"] + x["bet"] + 100 for e in seats for x in e["seats"]),
           "开局栈 = 剩余栈 + 盲注 + ante")

    # ante 比例不符：hand_start 之后紧跟 dead；hand_start 本身不合法（盲注坏）：只有 dead，hand_id 照带。
    play, sink = io.StringIO(), _EventSink()
    session, _, _ = _run_msgs(_with_ante(msgs, 2), ante=2, blueprint_ante=50, play_log=play,
                              play_events=sink)
    _check_events(play.getvalue(), sink.events, session)
    _check([e["type"] for e in sink.events] == ["hand_start", "dead"] * 8
           and all(e["tag"] == "ante_mismatch" for e in sink.events if e["type"] == "dead"),
           "ante 不符的事件序列")
    bad = [dict(m, blinds={"small_blind": 0, "big_blind": 2}) if m["type"] == "hand_start" else m
           for m in msgs]
    play, sink = io.StringIO(), _EventSink()
    session, _, _ = _run_msgs(bad, play_log=play, play_events=sink)
    _check_events(play.getvalue(), sink.events, session)
    _check([e["type"] for e in sink.events] == ["dead"] * 8
           and all(e["hand_id"] and e["tag"] == "bad_blinds" for e in sink.events),
           f"hand_start 不合法的事件 {sink.events[:2]}")

    # 事件文件写不进去：停写事件，行动文件 / play log 照旧。
    class _Broken:
        def write(self, rec):
            raise OSError(28, "No space left on device")
    play = io.StringIO()
    session, _, rows = _run_msgs(msgs, play_log=play, play_events=_Broken())
    _, _, rows0 = _run_msgs(msgs)
    _check(session.play_events is None and nots(rows) == nots(rows0)
           and len(play.getvalue().splitlines()) == len(ev),
           "事件文件写失败不该影响其余输出")

    # 路径缺省规则；off / 没有 play log 时不建文件。
    with tempfile.TemporaryDirectory() as d:
        pl = os.path.join(d, "play.log")
        _check(resolve_play_events(None, pl) == os.path.join(d, PLAY_EVENTS_NAME)
               and resolve_play_events("x.jsonl", pl) == "x.jsonl"
               and resolve_play_events("off", pl) is None and resolve_play_events(None, None) is None
               and resolve_play_events("off", None) is None, "--play-events 缺省规则")
        for arg, log in (("off", pl), (None, None)):
            f = open_play_events(arg, log)
            _check(f is None, "off / 无 play log 时不该打开事件文件")
            _run_msgs(msgs[:40], play_events=f)
        _check(os.listdir(d) == [], f"off 时不该写文件: {os.listdir(d)}")
        f = open_play_events(None, pl)
        _run_msgs(msgs[:40], play_events=f)
        f.close()
        with open(os.path.join(d, PLAY_EVENTS_NAME), encoding="utf-8") as fh:
            got = [json.loads(line) for line in fh]
        _check(got and got[0]["type"] == "hand_start", "缺省时应写到 play log 同目录")
    print(f"[selftest 13 行动指南事件流] {len(ev)} 条事件与 play log 逐行对应、字段齐、可还原文字；"
          "开关不影响其余输出；作废 / 过期 / 无建议 / 补盲 / ante / off 都覆盖", file=sys.stderr)


def run_selftest():
    _selftest_full_replay()
    _selftest_mid_hand_join()
    _selftest_tamper()
    _selftest_stale()
    _selftest_engine_rules()
    _selftest_post_rewrite()
    _selftest_legalize()
    _selftest_exploit_4bet()
    _selftest_hh_log()
    _selftest_token()
    _selftest_table_size()
    _selftest_ante()
    _selftest_posts()
    _selftest_play_events()
    _selftest_warm_hh()
    _selftest_chip_div()
    print("OK: wopo selftest 15 组场景全部通过。", file=sys.stderr)


# ===========================================================================
PLAY_EVENTS_NAME = "wopo_play_events.jsonl"


def resolve_play_events(arg, play_log):
    """--play-events 的实际路径：off → None；给了路径 → 原样；缺省时有 --play-log 就写它同目录的
    wopo_play_events.jsonl，没有就不写。"""
    if arg == "off":
        return None
    if arg:
        return arg
    if play_log:
        return os.path.join(os.path.dirname(os.path.abspath(play_log)), PLAY_EVENTS_NAME)
    return None


def open_play_events(arg, play_log):
    """按 resolve_play_events 打开事件文件（ActionFile：O_APPEND + 整行一次 write）；不写时 None。"""
    path = resolve_play_events(arg, play_log)
    return ActionFile(path) if path else None


WARM_HH_NAME = "wopo_hh.jsonl"


def resolve_warm_hh(arg, hh_log):
    """--warm-hh → glob 列表。arg = None（缺省）：有 --hh-log 时取它 run 目录的上一级下所有
    */wopo_hh.jsonl，否则不暖启动；["off"] → 不暖启动；其余 = 给定的 glob。"""
    if arg is None:
        if not hh_log:
            return []
        runs = os.path.dirname(os.path.dirname(os.path.abspath(hh_log)))
        return [os.path.join(runs, "*", WARM_HH_NAME)]
    if arg == ["off"]:
        return []
    return list(arg)


def load_warm_observes(patterns, amounts=False):
    """glob 匹配的 hh 文件 → observe 列表（按 ts 排序，hand_id 去重）+ 统计。坏行 / 缺字段的行跳过计数。"""
    import glob
    files = sorted({f for pat in patterns for f in glob.glob(pat)})
    stats = {"files": len(files), "hands": 0, "bad": 0, "dup": 0}
    seen, rows = set(), []
    for f in files:
        with open(f, encoding="utf-8") as fh:
            for line in fh:
                if not line.strip():
                    continue
                try:
                    rec = json.loads(line)
                    key = (rec.get("table_id"), rec["hand_id"])
                    obs = observe_from_hh(rec, amounts)
                except (ValueError, KeyError, TypeError):
                    stats["bad"] += 1
                    continue
                if key in seen:
                    stats["dup"] += 1
                    continue
                seen.add(key)
                rows.append((str(rec.get("ts") or ""), len(rows), obs))
    rows.sort(key=lambda r: (r[0], r[1]))
    stats["hands"] = len(rows)
    return [o for _, _, o in rows], stats


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
    p.add_argument("--play-events", default=None,
                   help="牌桌实况的结构化事件流（JSONL，append；opponent_analysis live --play-events 读，"
                        "网页「行动指南」）。缺省：给了 --play-log 时写同目录 wopo_play_events.jsonl，"
                        "否则不写；off 关闭")
    p.add_argument("--num-hands", type=int, default=0, help="打完这么多手后退出；0 = 不限")
    p.add_argument("--warm-hh", action="append", default=None,
                   help="画像暖启动的历史 hh（glob，可重复；off 关闭）。开了 --exploit / --exploit-4bet 时"
                        "默认开：--hh-log 所在 run 目录的上一级下所有 */wopo_hh.jsonl")
    p.add_argument("--ante", type=int, default=0,
                   help="桌面每座 ante（桌面筹码，默认 0）：开局人人先交，计入底池、不算本轮下注")
    p.add_argument("--blueprint-ante", type=int, default=0,
                   help="blueprint 的每座 ante（solver 筹码，BB = 100，= 训练时 train_cfr --ante；默认 0）。"
                        "每手要求 --ante × 100 == 本值 × 大盲")
    p.add_argument("--stack-bb", type=int, default=100,
                   help="blueprint 的起始码深（BB，= 训练时 train_cfr --stack-bb；默认 100）。透传给 advisor")
    add_fourbet_args(p)
    args = p.parse_args()
    # 根范围日志默认开（开了 --search 才有范围）：写在行动文件同目录的 wopo_ranges.jsonl；--range-log off 关。
    if args.range_log is None and args.search:
        args.range_log = os.path.join(os.path.dirname(os.path.abspath(args.action_file)), "wopo_ranges.jsonl")
    if args.ante < 0 or not 0 <= args.blueprint_ante <= 100:
        p.error("--ante 须 ≥ 0，--blueprint-ante 须在 [0, 100]")
    if not 2 <= args.stack_bb <= 1000:
        p.error("--stack-bb 须在 [2, 1000]")
    validate_advisor_args(args)
    validate_fourbet_args(p, args)
    token = None
    if not args.replay:
        token = load_token()
        if token is None:
            p.error(f"连 WS 需要 token：设环境变量 {TOKEN_ENV} 或写进 {ENV_FILE}（非空且不含空白）")

    # 画像暖启动的来源（参数检查放在启动 advisor 之前，报错不留 advisor 进程）。
    warm = resolve_warm_hh(args.warm_hh, args.hh_log)
    if warm and not (exploit_enabled(args) or fourbet_enabled(args)):
        if args.warm_hh is not None and args.warm_hh != ["off"]:
            p.error("--warm-hh 需要 --exploit 或 --exploit-4bet（没开画像）")
        warm = []

    ante_args = ["--ante", str(args.blueprint_ante)] if args.blueprint_ante else []
    # 缺省 100 不透传（advisor 默认 100，命令行与旧版逐字节相同）。
    stack_args = ["--stack-bb", str(args.stack_bb)] if args.stack_bb != 100 else []
    advisor = start_advisor(args, advisor_extra_args(args) + fourbet_extra_args(args) + ante_args
                            + stack_args)
    # 回放用同步 worker（结果确定、不会因为读文件太快把决策判成过期）；联机用后台线程。
    worker = DecisionWorker(advisor, sync=args.replay is not None)
    # 画像暖启动：第一手之前把历史 hh 当 observe 发给 advisor（与实盘每手结束发的同格式）。
    if warm:
        observes, st = load_warm_observes(warm, amounts=fourbet_enabled(args))
        for o in observes:
            worker.submit_observe(o)
        print(f"[warm] 画像暖启动：{st['files']} 个 hh 文件，{st['hands']} 手发给 advisor"
              f"（坏行 {st['bad']}，重复 {st['dup']}）；来源 {' '.join(warm)}", file=sys.stderr)
    else:
        print("[warm] 画像暖启动：关", file=sys.stderr)
    action_file = ActionFile(args.action_file)
    action_log = open(args.action_log, "a") if args.action_log else None
    hh_log = open(args.hh_log, "a", encoding="utf-8") if args.hh_log else None
    ws_log = WsLog(args.ws_log) if args.ws_log else None
    play_log = open(args.play_log, "a", encoding="utf-8") if args.play_log else None
    play_events = open_play_events(args.play_events, args.play_log)
    session = WopoSession(worker, action_file, n_seats=args.seats, hero_seat=HERO_SEAT,
                          num_hands=args.num_hands, action_log=action_log,
                          prewarm=args.search_prewarm, exploit=exploit_enabled(args),
                          debug=args.debug_log, exploit_4bet=fourbet_enabled(args), hh_log=hh_log,
                          play_log=play_log, ante=args.ante, blueprint_ante=args.blueprint_ante,
                          send_hand_id=args.range_log is not None, play_events=play_events)
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
        if play_events:
            play_events.close()
    if not ok:
        sys.exit(1)


if __name__ == "__main__":
    main()

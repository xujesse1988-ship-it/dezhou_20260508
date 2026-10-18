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
  - 桌面座位数 = --seats（默认 9，blueprint 须同座位数），至少 3 人发牌，否则略过该手。

行动文件每行字段：ts / hand_id / table_id / table_seq（触发决策的那条 table_state）/ street / hole /
board / pot / to_call / valid（can_check/can_call/can_raise/min_raise/max_raise，金额 = 本街总额）/
action（fold/check/call/raise/all_in）/ amount（raise 的本街总额，其余 null）/ probs（策略分布
[[动作, 概率]]，兜底动作为 null）/ chosen / source / decide_ms。

用法：
  wopo_play.py --url ws://HOST:PORT/... --action-file hero_actions.jsonl \\
      --cropped-checkpoint X.crop --bucket-table B.bin --search --search-trigger all-postflop ...
  wopo_play.py --replay session.log ...   回放 WS 日志（原始消息逐行，或 --ws-log 格式）
  wopo_play.py --selftest                 离线自测（stub advisor，不需要 blueprint）
"""

import argparse
import json
import os
import sys
import tempfile
import threading
import time

from poker_live.advisor_client import (add_advisor_args, advisor_extra_args, exploit_enabled,
                                       start_advisor, validate_advisor_args)
from poker_live.decision import DecisionWorker
from poker_live.table import HandEngine, HandInvalid
from poker_live.wopo import ActionFile, WopoSession
from poker_live.wslog import WsLog

HERO_SEAT = 0


# ===========================================================================
# 通讯
# ===========================================================================
def run_ws(session, url, ws_log=None, reconnect_wait=3):
    """被动连 wopo WS：收到的原始消息交给 session；断线 → session.on_disconnect + 等几秒重连。
    打满 --num-hands 或 Ctrl-C 结束。"""
    import websocket  # 延迟 import：回放 / selftest 不需要 websocket-client

    stopping = [False]

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
        if ws_log:
            ws_log.event("error", error=repr(err))

    def on_close(ws, code, reason):
        print(f"  [ws closed] code={code} reason={reason}", file=sys.stderr)
        if ws_log:
            ws_log.event("close", code=code,
                         reason=reason if isinstance(reason, str) else repr(reason))

    while True:
        if ws_log:
            ws_log.conn += 1
        ws = websocket.WebSocketApp(url, on_open=on_open, on_message=on_message,
                                    on_error=on_error, on_close=on_close)
        try:
            ws.run_forever(ping_interval=30, ping_timeout=10)
        except KeyboardInterrupt:
            stopping[0] = True
        session.on_disconnect()
        if stopping[0] or session.done:
            return
        print(f"  断线，{reconnect_wait}s 后重连…", file=sys.stderr)
        try:
            time.sleep(reconnect_wait)
        except KeyboardInterrupt:
            return


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


class _StubAdvisor:
    """确定性合法动作（check → call → fold）+ 记录请求。gate 非 None 时 decide 阻塞到 gate 被 set
    （模拟搜索慢于 hero 出牌）。"""

    def __init__(self, gate=None):
        self.requests = []
        self.side = []
        self.gate = gate

    def decide(self, req):
        if self.gate is not None:
            self.gate.wait(10)
        self.requests.append(req)
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


def _run_msgs(msgs, gate=None, sync=True, hook=None, exploit=False, prewarm=False):
    """msgs（dict 或 ("disconnect",)）喂一个新会话。返回 (session, stub, 行动文件行)。
    hook(i, session) 在每条消息后调用（异步场景用来在指定位置放行 advisor）。"""
    stub = _StubAdvisor(gate)
    worker = DecisionWorker(stub, sync=sync)
    with tempfile.TemporaryDirectory() as d:
        path = os.path.join(d, "actions.jsonl")
        af = ActionFile(path)
        session = WopoSession(worker, af, n_seats=9, hero_seat=HERO_SEAT,
                              exploit=exploit, prewarm=prewarm)
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
    session, stub, rows = _run_msgs(msgs, exploit=True, prewarm=True)
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


def run_selftest():
    _selftest_full_replay()
    _selftest_mid_hand_join()
    _selftest_tamper()
    _selftest_stale()
    _selftest_engine_rules()
    print("OK: wopo selftest 5 组场景全部通过。", file=sys.stderr)


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
    src = p.add_mutually_exclusive_group(required=True)
    src.add_argument("--url", help="wopo WS 地址")
    src.add_argument("--replay", help="回放文件（原始消息逐行，或 --ws-log 格式）")
    p.add_argument("--action-file", required=True,
                   help="行动文件（外部程序读）：每个决策点追加一行 JSON")
    p.add_argument("--action-log", default="wopo_actions_log.jsonl",
                   help="每决策落 {req,resp,decide_ms,stale} JSONL（含过期的）；空串关闭")
    p.add_argument("--ws-log", default=None, help="原始 WS 消息逐条落 JSONL（append）；缺省关")
    p.add_argument("--num-hands", type=int, default=0, help="打完这么多手后退出；0 = 不限")
    args = p.parse_args()
    validate_advisor_args(args)

    advisor = start_advisor(args, advisor_extra_args(args))
    # 回放用同步 worker（结果确定、不会因为读文件太快把决策判成过期）；联机用后台线程。
    worker = DecisionWorker(advisor, sync=args.replay is not None)
    action_file = ActionFile(args.action_file)
    action_log = open(args.action_log, "a") if args.action_log else None
    ws_log = WsLog(args.ws_log) if args.ws_log else None
    session = WopoSession(worker, action_file, n_seats=args.seats, hero_seat=HERO_SEAT,
                          num_hands=args.num_hands, action_log=action_log,
                          prewarm=args.search_prewarm, exploit=exploit_enabled(args),
                          debug=args.debug_log)
    try:
        if args.replay:
            run_replay(session, args.replay)
        else:
            run_ws(session, args.url, ws_log=ws_log)
    finally:
        worker.close(timeout=30)
        advisor.close()
        session.report()
        action_file.close()
        if action_log:
            action_log.close()
        if ws_log:
            ws_log.close()


if __name__ == "__main__":
    main()

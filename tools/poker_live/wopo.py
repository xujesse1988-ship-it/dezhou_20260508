"""wopo 牌桌服务：消息适配 + 会话（只收不发）。

数据流：transport（wopo_play.py：WS / 回放文件）→ WopoSession.on_raw → 适配成规范化事件 →
table.HandEngine 推进 + 校验 → 轮到 hero → decision.DecisionWorker 调 advisor → 行动文件。

协议（session-20261013-141613-936129.log 实测）：hand_start / hole_cards / table_state /
player_action / community_cards / hand_result；没有 your_turn —— 轮到 hero = table_state 的
actor_seat == hero 座。hero 实际怎么打以服务器回来的 player_action 为准（可能和建议不同），
历史只记服务器的动作。中途接入 / 断线 / 消息对不上 → 这手作废，等下一个 hand_start。
"""

import json
import os
import sys
import threading
import time

from poker_live.decision import make_request, safe_response, tally_source
from poker_live.table import HandEngine, HandInvalid

# 已知消息类型；其余类型首见时打 stderr、忽略（不作废当前手）。
_KNOWN_TYPES = ("hand_start", "hole_cards", "table_state", "player_action",
                "community_cards", "hand_result")


_STREET_CN = {"preflop": "翻前", "flop": "翻牌", "turn": "转牌", "river": "河牌"}
_STALE_CN = {"hero_acted": "hero 已行动", "hand_over": "这手已结束", "hand_dead": "这手已作废"}


def _fmt_act(action, to=None, delta=None, to_call=None):
    """play log 动作文字：raise / all_in 带本街总额（前面没人下注的 raise 写成 bet），call 带跟的额。"""
    if action == "raise" and to_call == 0:
        return f"bet {to}"
    if to is not None:
        return f"{action} 到 {to}"
    return f"{action} {delta}" if action == "call" and delta is not None else action


class ActionFile:
    """行动文件（外部程序读）：每个决策点追加一行 JSON。O_APPEND + 整行一次 write，
    读方 tail 时不会读到被别的写入插进来的半行。"""

    def __init__(self, path):
        self._fd = os.open(path, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o644)

    def write(self, rec):
        os.write(self._fd, (json.dumps(rec, ensure_ascii=False) + "\n").encode())

    def close(self):
        os.close(self._fd)


def _snapshot(msg):
    """wopo table_state → HandEngine 快照（座位键转 int；坏结构 → HandInvalid）。"""
    seats = msg.get("seats")
    if not isinstance(seats, list):
        raise HandInvalid("snapshot_no_seats")
    out = {}
    for s in seats:
        if not isinstance(s, dict) or not isinstance(s.get("seat"), int):
            raise HandInvalid("snapshot_bad_seat")
        out[s["seat"]] = s
    return {"street": msg.get("street"), "board": msg.get("board"), "pot": msg.get("pot"),
            "actor": msg.get("actor_seat"), "seats": out, "n_seats": len(seats)}


def _action(msg):
    """wopo player_action → HandEngine 动作。raise 的 amount 是本街总额（amount_mode=to_total）；
    其他 amount_mode 的 amount 不当 to 用（投入以 contribution_delta 为准）。"""
    to = msg.get("amount") if msg.get("amount_mode") == "to_total" else None
    return {"seat": msg.get("seat"), "action": msg.get("action"), "street": msg.get("street"),
            "delta": msg.get("contribution_delta"), "to": to,
            "stack_before": msg.get("stack_before"), "stack_after": msg.get("stack"),
            "pot_before": msg.get("pot_before"), "pot_after": msg.get("pot"),
            "name": msg.get("name")}


def legalize(resp, valid):
    """advisor 响应 → 行动文件可用的合法动作（同 poker_manager driver 的处理）。
    - 不能加注时 advisor 回不带金额的 all_in（对手已全下、hero 更深）：能跟注就改成 call，
      source 加 :allin_as_call——跟注就是这时能投入的最多筹码。
    - 其余不在合法区间里的动作 → 安全动作兜底（fallback:illegal_advisor_action），rejected 记原回复。"""
    action, amount = resp.get("action"), resp.get("amount")
    if action == "all_in" and not valid.get("can_raise") and valid.get("can_call"):
        return {**resp, "action": "call", "source": f"{resp.get('source', '')}:allin_as_call"}
    if action == "raise":
        ok = (valid.get("can_raise") and not isinstance(amount, bool)
              and isinstance(amount, (int, float)) and int(amount) == amount
              and valid["min_raise"] <= amount <= valid["max_raise"])
    else:
        ok = (action == "fold" or (action == "check" and valid.get("can_check"))
              or (action == "call" and valid.get("can_call"))
              or (action == "all_in" and valid.get("can_raise")))
    if ok:
        return resp
    out = safe_response(valid, "fallback:illegal_advisor_action")
    out["rejected"] = {k: resp.get(k) for k in ("action", "amount", "source")}
    return out


class WopoSession:
    def __init__(self, worker, action_file, n_seats, hero_seat=0, num_hands=0,
                 action_log=None, prewarm=False, exploit=False, debug=False,
                 exploit_4bet=False, hh_log=None, play_log=None):
        self.worker = worker
        self.action_file = action_file
        self.n_seats = n_seats
        self.hero_seat = hero_seat
        self.num_hands = num_hands  # 0 = 不限
        self.action_log = action_log
        self.prewarm = prewarm
        self.exploit = exploit
        # PM-17 (--exploit-4bet)：advisor 要请求里的玩家名 + 每手带金额的 observe（同 poker_manager）。
        self.exploit_4bet = exploit_4bet
        self.send_names = exploit or exploit_4bet
        self.hh_log = hh_log    # 校验通过的整手牌局 JSONL（opponent_analysis 读）；None = 不写
        # 牌桌实况（人看）：手牌 / 公共牌 / 玩家行动 / hero 决策 / 结算 / 作废；None = 不写。
        # 诊断信息（连接、未知消息、过期等）照旧走 stderr。
        self.play_log = play_log
        self.debug = debug
        # 消息处理线程（transport）和决策回调线程（worker）共用 → 串行化；RLock：同步 worker
        # 在处理线程里当场回调。
        self.lock = threading.RLock()
        self.hand = None        # 当前 HandEngine（作废 / 结束后保留到下一个 hand_start，挡掉尾随快照）
        self.dead = None        # 当前手作废原因；None = 正常
        self.table_id = None
        self.last_seq = None
        self.done = False       # 打满 num_hands
        self._seen_types = set()
        self._decided = set()   # 已提交决策的 turn_key（快照重复推送只决策一次）
        self._advice = {}       # turn_key → 已写进行动文件的建议（和 hero 实际动作对照）
        # turns = 提交的决策数；decisions = 已返回的（差值 = advisor 还在算）。
        self.counters = {"hands": 0, "hands_complete": 0, "turns": 0, "decisions": 0, "written": 0,
                         "stale": 0, "blueprint": 0, "search": 0, "limp_heuristic": 0,
                         "fallback": 0, "prewarms": 0, "hero_actions": 0, "hero_same": 0,
                         "hero_diff": 0, "hero_unadvised": 0, "dup_msgs": 0, "result_notes": 0}
        self.fallback_reasons = {}
        self.skip_reasons = {}  # 作废原因（冒号前标签）→ 手数

    def _play(self, text):
        if self.play_log:
            self.play_log.write(f"{time.strftime('%H:%M:%S')} {text}\n")
            self.play_log.flush()

    def _who(self, seat):
        name = self.hand.names.get(seat) if self.hand else None
        return f"seat{seat} {name}" if name else f"seat{seat}"

    def _dbg(self, msg):
        if self.debug:
            print(f"  [dbg] {msg}", file=sys.stderr)

    # ---- transport 入口 ----
    def on_raw(self, raw):
        with self.lock:
            try:
                msg = json.loads(raw)
            except (ValueError, TypeError):
                self._kill("bad_json")
                print(f"  [msg] 非 JSON 消息: {str(raw)[:200]!r}", file=sys.stderr)
                return
            if not isinstance(msg, dict):
                self._kill("bad_json")
                return
            try:
                self._handle(msg)
            except Exception as e:
                # 消息结构超出预期（字段类型不对等）：不能让异常打断连接，这手作废、继续收。
                print(f"  [msg] 处理异常 {e!r}: {str(raw)[:200]!r}", file=sys.stderr)
                self._kill(f"internal_error:{type(e).__name__}")

    def on_disconnect(self):
        """连接断开：没有补发机制，当前手作废；序号重新起算。"""
        with self.lock:
            self._kill("disconnected")
            self.last_seq = None

    # ---- 内部 ----
    def _alive(self):
        return self.hand is not None and self.dead is None and not self.hand.finished

    def _kill(self, reason):
        if not self._alive():
            return
        self.dead = reason
        self._count_skip(self.hand.hand_id, reason)

    def _count_skip(self, hand_id, reason):
        tag = reason.split(":", 1)[0]
        self.skip_reasons[tag] = self.skip_reasons.get(tag, 0) + 1
        print(f"  [skip] hand={hand_id} 作废：{reason}", file=sys.stderr)
        self._play(f"本手作废：{reason}（之后不出建议）")

    def _handle(self, msg):
        t = msg.get("type")
        if t not in self._seen_types:
            self._seen_types.add(t)
            if t not in _KNOWN_TYPES:
                print(f"  [msg] 未处理消息类型 {t!r} keys={sorted(msg.keys())}", file=sys.stderr)
        tid = msg.get("table_id")
        if tid and tid != self.table_id:
            if self.table_id is not None:
                self._kill(f"table_changed:{self.table_id}->{tid}")
            print(f"  [table] table_id={tid}", file=sys.stderr)
            self.table_id = tid
            self.last_seq = None
        seq = msg.get("table_seq")
        if isinstance(seq, int):
            if self.last_seq is not None and seq <= self.last_seq:
                self.counters["dup_msgs"] += 1
                return
            if self.last_seq is not None and seq != self.last_seq + 1:
                self._kill(f"table_seq_gap:{self.last_seq}->{seq}")
            self.last_seq = seq

        if t == "hand_start":
            self._on_hand_start(msg)
            return
        if t not in _KNOWN_TYPES or not self._alive():
            return
        hand = self.hand
        if msg.get("hand_id") != hand.hand_id:
            self._kill(f"hand_id_mismatch:{msg.get('hand_id')}")
            return
        try:
            if t == "hole_cards":
                first = hand.hole is None
                hand.apply_hole(msg.get("cards"))
                if first:
                    self._play(f"底牌 {' '.join(hand.hole)}")
                self._dbg(f"hole {' '.join(hand.hole)}")
                self._maybe_decide(msg)  # 手牌晚于轮次快照到达时补上决策
            elif t == "table_state":
                started = hand.started
                hand.apply_snapshot(_snapshot(msg))
                if not started:
                    self._play_seats()
                self._maybe_decide(msg)
            elif t == "player_action":
                self._on_action(msg)
            elif t == "community_cards":
                hand.apply_board(msg.get("street"), msg.get("cards"))
                self._play(f"{_STREET_CN[hand.street]} {' '.join(hand.board)}")
                self._dbg(f"{hand.street} board={' '.join(hand.board)}")
                self._maybe_prewarm()
            elif t == "hand_result":
                self._on_result(msg)
        except HandInvalid as e:
            self._kill(str(e))

    def _on_hand_start(self, msg):
        if self._alive():
            self._kill("next_hand_started")
        self.hand = None
        self.dead = None
        self._decided.clear()
        self._advice.clear()
        self.counters["hands"] += 1
        blinds = msg.get("blinds") if isinstance(msg.get("blinds"), dict) else {}
        try:
            self.hand = HandEngine(msg.get("hand_id"), msg.get("dealer_seat"),
                                   blinds.get("small_blind"), blinds.get("big_blind"),
                                   self.hero_seat, self.n_seats)
        except HandInvalid as e:
            self._count_skip(msg.get("hand_id"), str(e))  # hand 留 None：本手后续消息全忽略
            return
        if msg.get("seat") is not None and msg.get("seat") != self.hero_seat:
            self._kill(f"hero_seat:{msg.get('seat')}!={self.hero_seat}")
            return
        self._play(f"──── {msg.get('hand_id')} 庄位 seat{self.hand.button} "
                   f"盲注 {self.hand.sb}/{self.hand.bb}")
        self._dbg(f"=== hand_start hand_id={msg.get('hand_id')} button={msg.get('dealer_seat')} "
                  f"blinds={blinds.get('small_blind')}/{blinds.get('big_blind')} ===")

    def _play_seats(self):
        """首个快照后：发牌座 + 开局栈（含盲注）+ 庄 / 盲位。"""
        hand = self.hand
        tag = {hand.button: "BTN", hand.sb_seat: "SB", hand.bb_seat: "BB"}
        self._play("座位 " + " | ".join(
            f"{self._who(s)} {hand.start_stack[s]}" + (f"({tag[s]})" if s in tag else "")
            for s in hand.dealt))

    def _on_action(self, msg):
        hand = self.hand
        key = hand.turn_key()
        hand.apply_action(_action(msg))
        a = hand.actions[-1]
        self._dbg(f"act seat={a['seat']} {a['action']}" + (f" to={a['to']}" if "to" in a else ""))
        amt = hand.action_amounts[-1]
        line = f"{self._who(a['seat'])} " + _fmt_act(a["action"], a.get("to"),
                                                     amt["contribution_delta"], amt["to_call_before"])
        if a["seat"] != self.hero_seat:
            self._play(line)
            return
        # hero 实际动作（服务器为准）和本程序的建议对照。
        self.counters["hero_actions"] += 1
        adv = self._advice.get(key)
        if adv is None:
            # 建议没赶上（搜索还在跑 = hero 已自行决策）或没触发决策。
            self.counters["hero_unadvised"] += 1
            self._play(f"{line}  ← hero 实际行动（无建议）")
            return
        same = adv["action"] == a["action"] or (
            adv["action"] in ("raise", "all_in") and a["action"] in ("raise", "all_in"))
        self.counters["hero_same" if same else "hero_diff"] += 1
        self._play(f"{line}  ← hero 实际行动（{'同' if same else '不同于'}建议 {adv['text']}）")
        if not same:
            self._dbg(f"hero 实际 {a} ≠ 建议 {adv}")

    def _on_result(self, msg):
        hand = self.hand
        notes = hand.apply_result(msg.get("final_stacks"))
        final = msg.get("final_stacks") or {}
        net = []
        for s in hand.dealt:
            v = final.get(str(s), final.get(s))
            if isinstance(v, (int, float)) and v != hand.start_stack[s]:
                net.append(f"{self._who(s)} {int(v) - hand.start_stack[s]:+d}")
        self._play("结算 " + (" | ".join(net) or "无输赢"))
        self.counters["hands_complete"] += 1
        if notes:
            self.counters["result_notes"] += 1
            print(f"  [result] hand={hand.hand_id} 结算核对：{'; '.join(notes)}", file=sys.stderr)
        if self.send_names:
            self.worker.submit_observe(hand.observe_record(amounts=self.exploit_4bet))
        if self.hh_log:
            # 走到这里的手都没作废（作废的手不再处理后续消息）；结算核对不过也照写，记 result_notes，
            # opponent_analysis 回放时会再校验一遍并拒收。
            rec = {"wopo_hh": 1, "table_id": self.table_id, "ts": msg.get("ts"),
                   **hand.hh_record(), "result": msg, "result_notes": notes}
            self.hh_log.write(json.dumps(rec, ensure_ascii=False) + "\n")
            self.hh_log.flush()
        self._dbg(f"hand_result winners={msg.get('winners')} final_stacks={msg.get('final_stacks')}")
        if self.num_hands and self.counters["hands_complete"] >= self.num_hands:
            self.done = True

    def _request(self, valid):
        hand = self.hand
        dealt = hand.dealt if len(hand.dealt) < self.n_seats else None
        return make_request(hand.hole, hand.board, hand.button, hand.hero, self.n_seats,
                            hand.sb, hand.bb, list(hand.actions), valid,
                            stacks=hand.hand_start_stacks(placeholder=100 * hand.bb),
                            dealt_seats=list(dealt) if dealt else None,
                            names=dict(hand.names) if self.send_names else None)

    def _maybe_prewarm(self):
        """街起点预热（--search-prewarm）：hero 本手还能行动才发；发错无害（advisor 侧 skip）。"""
        hand = self.hand
        if not self.prewarm or not hand.hole or hand.hero in hand.folded \
                or hand.stack[hand.hero] == 0:
            return
        stub = {"can_check": False, "can_call": False, "can_raise": False,
                "min_raise": None, "max_raise": None}
        self.worker.submit_prewarm(self._request(stub))
        self.counters["prewarms"] += 1

    def _maybe_decide(self, msg):
        hand = self.hand
        valid = hand.hero_turn()
        if valid is None:
            return
        key = hand.turn_key()
        if key in self._decided:
            return
        self._decided.add(key)
        self.counters["turns"] += 1
        req = self._request(valid)
        ctx = {"hand": hand, "key": key, "table_seq": msg.get("table_seq"),
               "street": hand.street, "pot": hand.pot, "to_call": hand.to_call(hand.hero),
               "board": list(hand.board), "valid": valid, "t_wall": time.time()}
        self._dbg(f"your_turn street={hand.street} hole={''.join(hand.hole)} "
                  f"board={' '.join(hand.board)} valid={valid}")
        self.worker.submit_decide(req, lambda resp, ms: self._on_decided(req, ctx, resp, ms))

    def _on_decided(self, req, ctx, resp, ms):
        """决策结果（worker 线程回调）。hero 在此期间已出牌 / 这手已作废 → 结果过期，不写行动文件。"""
        with self.lock:
            self.counters["decisions"] += 1
            resp = legalize(resp, ctx["valid"])
            tally_source(self.counters, self.fallback_reasons, resp.get("source", ""))
            hand = self.hand
            stale = None
            if hand is not ctx["hand"]:
                stale = "hand_over"
            elif self.dead is not None:
                stale = f"hand_dead:{self.dead}"
            elif hand.finished or hand.turn_key() != ctx["key"]:
                stale = "hero_acted"
            advice = _fmt_act(resp.get("action"), resp.get("amount"), ctx["to_call"], ctx["to_call"])
            rec = {"ts": round(time.time(), 3), "hand_id": ctx["key"][0], "table_id": self.table_id,
                   "table_seq": ctx["table_seq"], "street": ctx["street"],
                   "hole": req["hole"], "board": ctx["board"], "pot": ctx["pot"],
                   "to_call": ctx["to_call"], "valid": ctx["valid"],
                   "action": resp.get("action"), "amount": resp.get("amount"),
                   "probs": resp.get("probs"), "chosen": resp.get("chosen"),
                   "source": resp.get("source"), "decide_ms": ms}
            if stale is None:
                self.action_file.write(rec)
                self._advice[ctx["key"]] = {"action": rec["action"], "amount": rec["amount"],
                                            "text": advice}
                self.counters["written"] += 1
            else:
                self.counters["stale"] += 1
                print(f"  [stale] hand={ctx['key'][0]} 决策 {ms}ms 返回时已过期（{stale}），"
                      f"不写行动文件", file=sys.stderr)
            self._play(f">> 轮到 hero（{_STREET_CN[ctx['street']]}，底池 {ctx['pot']}，跟 {ctx['to_call']}）"
                       f"建议 {advice}  来源 {rec['source']}  {ms}ms"
                       + (f"  已过期（{_STALE_CN.get(stale.split(':', 1)[0], stale)}），未写入行动文件"
                          if stale else ""))
            if self.action_log:
                self.action_log.write(json.dumps({"req": req, "resp": resp, "decide_ms": ms,
                                                  "stale": stale}, ensure_ascii=False) + "\n")
                self.action_log.flush()

    def report(self):
        c = self.counters
        d = c["decisions"]
        print(f"hands={c['hands']}（打完 {c['hands_complete']}，作废 {sum(self.skip_reasons.values())}）"
              f" decisions={d} written={c['written']} stale={c['stale']} "
              f"blueprint={c['blueprint']} search={c['search']} "
              f"limp_heuristic={c['limp_heuristic']} fallback={c['fallback']} "
              f"({100.0 * c['fallback'] / d if d else 0:.1f}% 兜底) prewarms={c['prewarms']} | "
              f"hero 实际动作 {c['hero_actions']}：同建议 {c['hero_same']} / 不同 {c['hero_diff']} / "
              f"无建议 {c['hero_unadvised']}", file=sys.stderr)
        if self.skip_reasons:
            print("  作废原因（降序）:", file=sys.stderr)
            for k, v in sorted(self.skip_reasons.items(), key=lambda kv: -kv[1]):
                print(f"    {v:>5}× {k}", file=sys.stderr)
        if self.fallback_reasons:
            print("  兜底/giveup 原因（降序）:", file=sys.stderr)
            for k, v in sorted(self.fallback_reasons.items(), key=lambda kv: -kv[1]):
                print(f"    {v:>5}× {k}", file=sys.stderr)

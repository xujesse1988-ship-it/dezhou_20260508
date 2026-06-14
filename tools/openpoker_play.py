#!/usr/bin/env python3
"""OpenPoker 6-max WS driver（docs/temp/openpoker_client_design_2026_06_02.md §2/§3/§7）。

网络 / token / 重连 / 限速全在本文件（Python，websocket-client），策略全在常驻 Rust
advisor（tools/openpoker_advisor.rs）—— Rust crate 零网络依赖（invariant）。

driver 职责（§3）：
  - WS 连 wss://openpoker.ai/ws + Bearer 鉴权；join_lobby{buy_in:2000}（锁 100BB）。
  - 累计每手 betting 历史（hand_start/hole_cards/player_action/community_cards）→ 组 advisor 请求。
  - your_turn → 调 advisor 拿 {action, amount} → 回 action{turn_token, client_action_id}。
  - §4 码深漂移：每手后我方栈漂出 [80,125]BB → leave_table + rejoin 取 2000（控我方栈）。
  - §4 两人桌：一手仅 2 座发牌（HU）→ 本实现 postflop 不支持 → leave_table + 离线等 10 分钟
    重连，**连续** 5 次仍是两人桌才放弃（中途换到人多的桌即清零；HU 是 live 最大出血点，exec §3.2）。
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

# 两人桌（HU）处理：本实现 postflop 不支持两人（HU 是 live 最大出血点，exec §3.2 ——
# 6-max 树表达不了跨街反转、smoke 实测 87.5% 兜底）。检测到一手仅 2 座发牌 → 主动离场，
# 等 HU_RETRY_WAIT_S 后重连（期望换到人更多的桌）；**连续** HU_MAX_RETRIES 次仍是两人桌才放弃
# （重连后只要落到人多的桌就清零，不累积）。
HU_RETRY_WAIT_S = 600   # 10 分钟。
HU_MAX_RETRIES = 5

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
        # 未读取的 prewarm 响应行数（prewarm 先发后弃响应；管道有序 → decide 前按计数 drain
        # 即可对齐，不会把预热响应误当决策响应）。
        self._pending_prewarms = 0

    def prewarm(self, req):
        """RoundStart 预热（--search-prewarm）：街起点、hero 行动**前**发 prewarm 请求，让
        advisor 把该街 solve 提前算进缓存（build+solve wall 藏进对手行动时间）。**不等响应**
        （等 = 阻塞 WS 消息处理，预热就白做了）；响应行在下次 decide 前 drain 丢弃。"""
        payload = dict(req)
        payload["prewarm"] = True
        self.proc.stdin.write(json.dumps(payload) + "\n")
        self.proc.stdin.flush()
        self._pending_prewarms += 1

    def _drain_pending(self):
        while self._pending_prewarms > 0:
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError("advisor 无响应（退出？）")
            self._pending_prewarms -= 1

    def decide(self, req):
        """req = dict（见 openpoker_advisor::Request）。返回 advisor 响应 dict
        {action, amount?, source, ...}。"""
        self._drain_pending()
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
def _commit_step(committed, seat, a, amount):
    """单步本街 committed 转移（on_player_action 增量路径与短桌投入重算共用，保两边不漂）。
    返回 (to, delta)：to = raise/bet 的本街累计到额；delta = 本动作投入增量。"""
    prev = committed.get(seat, 0)
    to = None
    if a in ("raise", "bet"):
        # [LIVE?] 视 amount 为总 to 额；若实为增量改成 committed[seat] + amount。
        committed[seat] = amount if amount is not None else prev
        to = committed[seat]
    elif a == "call":
        committed[seat] = max(committed.values())
    elif a == "all_in":
        if amount is not None:
            committed[seat] = amount
    # check/fold：committed 不变。
    return to, committed.get(seat, 0) - prev


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
        # 本街每座累计投入（preflop 含盲注）；新街清零。盲注 seeding 按满桌 btn+1/+2 ——
        # **短桌手必错**（盲注跳空座），短桌路径不读它、用 _committed_total_for 按 dealt 重算。
        self.committed = {s: 0 for s in range(NUM_SEATS)}
        self.committed[(button_seat + 1) % NUM_SEATS] = SMALL_BLIND_OP  # SB
        self.committed[(button_seat + 2) % NUM_SEATS] = BIG_BLIND_OP    # BB
        # 缺口②：本手累计投入（跨街**不清零**，含盲注）→ 回推 hand-start 真栈：
        # hand_start[s] = your_turn 给的当前 remaining[s] + committed_total[s]。
        self.committed_total = dict(self.committed)
        # 各座当前 remaining 栈（OpenPoker 单位）；从 your_turn.players[].stack / player_action.stack
        # 滚动更新（None = 还没观测到该座栈）。
        self.stacks_now = {s: None for s in range(NUM_SEATS)}
        # 短桌占座推断（幻影座映射，exec §3.2）：本手发牌座 = table_state.seats[].in_hand 累计
        # ∪ 已行动座 ∪ {我, button}。your_turn.players 含「在座未发牌」的等局玩家（live 625 手
        # 实测 20 手虚座）不可作依据；仅本手收到过 table_state（dealt_confirmed）才可判。
        self.in_hand_seen = set()
        self.acted = set()
        self.dealt_confirmed = False
        # 原始动作事件流（("act",seat,action,amount) / 街变 ("street",)）：短桌投入重算的输入。
        self._raw_events = []

    def on_player_action(self, seat, action, amount, ext=None):
        """记一条对手 / 我方已确认动作。to = 该座本街累计到额（raise/bet 才需）。
        ext = player_action 原始字段子集（HH 日志用，advisor 不读）。"""
        a = (action or "").lower()
        to, delta = _commit_step(self.committed, seat, a, amount)
        # 缺口②：本手累计投入 += 本动作增量（本街新 committed − 旧）。all_in 无 amount → 增量 0
        # （信息缺，advisor 真栈重放会因 apply 非法回落 fold，不污染 blueprint 路径）。
        self.committed_total[seat] = self.committed_total.get(seat, 0) + delta
        self.actions.append({"seat": seat, "action": a, **({"to": to} if to is not None else {})})
        self.actions_ext.append(dict(ext) if ext else {})
        if seat is not None:
            self.acted.add(seat)
        self._raw_events.append(("act", seat, a, amount))

    def update_stacks(self, seat, stack):
        """从 player_action.stack / your_turn.players[].stack 滚动记各座当前 remaining 栈。
        int(round(.)) 归一：服务端 JSON 可能给 float（2000.0），advisor Request.stacks 是 u64。"""
        if seat is not None and stack is not None and 0 <= seat < NUM_SEATS:
            self.stacks_now[seat] = int(round(stack))

    def update_name(self, seat, name):
        """从 your_turn.players[].name 记 seat→name（HH 日志：对手可追踪性，§4.2 目的 2）。"""
        if seat is not None and name is not None and 0 <= seat < NUM_SEATS:
            self.names[seat] = name

    def dealt_now(self):
        """决策时占座推断（短桌幻影座映射）。None = 不可判（本手没收到过 table_state）→
        维持满桌假设（advisor 走旧路径，短桌手照旧 seat_mismatch 兜底）。"""
        if not self.dealt_confirmed:
            return None
        d = set(self.in_hand_seen) | set(self.acted)
        if self.my_seat is not None:
            d.add(self.my_seat)
        if self.button_seat is not None:
            d.add(self.button_seat)  # live 625 手实测 button 恒为发牌座（HH remap 0 失败）。
        return {s for s in d if isinstance(s, int) and 0 <= s < NUM_SEATS}

    def _blind_seats(self, dealt_sorted):
        """短桌真实盲注座 = button 起顺时针前两个**发牌座**。OpenPoker 对 k=2 也用统一环规则
        （live 校准 2026-06-11 smoke：HU 时 button 发 BB、非 button 发 SB 先动——非标准 HU），
        所以不设特例：k=2 时 (btn+1)%2=对手=SB、(btn+2)%2=button=BB。
        button 不在 dealt → ValueError（caller 退满桌假设）。"""
        k = len(dealt_sorted)
        bi = dealt_sorted.index(self.button_seat)
        return dealt_sorted[(bi + 1) % k], dealt_sorted[(bi + 2) % k]

    def _committed_total_for(self, sb_seat, bb_seat):
        """按给定盲注座从原始事件流重算本手累计投入（短桌：__init__ 的满桌 btn+1/+2 seeding
        必错，按 dealt 事后重算；满桌路径不读本函数）。转移与 on_player_action 共用 _commit_step。"""
        committed = {s: 0 for s in range(NUM_SEATS)}
        committed[sb_seat] += SMALL_BLIND_OP
        committed[bb_seat] += BIG_BLIND_OP
        total = dict(committed)
        for ev in self._raw_events:
            if ev[0] == "street":
                committed = {s: 0 for s in range(NUM_SEATS)}
                continue
            _, seat, a, amount = ev
            _, delta = _commit_step(committed, seat, a, amount)
            total[seat] = total.get(seat, 0) + delta
        return total

    def hand_start_stacks(self):
        """回推各座 hand-start 真栈（OpenPoker 单位）= 当前 remaining + 本手累计投入。
        满桌（或占座不可判）：全 6 座栈都已观测到才返回长 6 list（旧行为，byte-equal）。
        短桌（dealt 可判且 k<6）：只需**发牌座**已观测；投入按 dealt ring 的真实盲注座重算；
        非发牌座填 BUY_IN placeholder（advisor 按 dealt_seats 忽略，HH 解析侧短桌本就不读）。"""
        dealt = self.dealt_now()
        if dealt is None or len(dealt) >= NUM_SEATS:
            if any(self.stacks_now[s] is None for s in range(NUM_SEATS)):
                return None
            return [self.stacks_now[s] + self.committed_total.get(s, 0) for s in range(NUM_SEATS)]
        ds = sorted(dealt)
        if len(ds) < 2 or any(self.stacks_now[s] is None for s in ds):
            return None
        try:
            sb_seat, bb_seat = self._blind_seats(ds)
        except ValueError:
            return None
        total = self._committed_total_for(sb_seat, bb_seat)
        return [(self.stacks_now[s] + total.get(s, 0)) if s in dealt else BUY_IN
                for s in range(NUM_SEATS)]

    def on_community(self, cards, street):
        self.board = cards
        self.street = street
        # 新街：本街投入清零（§3）；committed_total 跨街保留（缺口②）。
        self.committed = {s: 0 for s in range(NUM_SEATS)}
        self._raw_events.append(("street",))

    def build_request(self, valid):
        """组 advisor 请求（openpoker_advisor::Request）。`stacks` 仅在真栈已知时附带
        （缺口②实时搜索读；缺省 → advisor 退对称 100BB blueprint，byte-equal）。
        `dealt_seats` 仅短桌且占座可判时附带（幻影座映射；满桌不带 = 请求 byte-equal 旧行为）。"""
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
        dealt = self.dealt_now()
        if dealt is not None and len(dealt) < NUM_SEATS:
            req["dealt_seats"] = sorted(dealt)
        return req

    def hh_record(self, hand_result_msg):
        """一手 HH JSONL 记录（§4.2 数据管道）。hand_result 的 winners/final_stacks/
        shown_cards/pot **原样保留**（不做有损映射，单位换算在 Rust 解析侧）；只读状态不写。
        dealt_est = 决策时占座推断（事后可对 final_stacks 键集验证推断质量）。"""
        dealt = self.dealt_now()
        committed_total = self.committed_total
        if dealt is not None and 2 <= len(dealt) < NUM_SEATS:
            try:
                committed_total = self._committed_total_for(*self._blind_seats(sorted(dealt)))
            except ValueError:
                pass  # button 不在推断 dealt 内：保留满桌口径（解析侧短桌本就不读）。
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
            "committed_total": committed_total,
            "dealt_est": sorted(dealt) if dealt is not None else None,
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
                    "lobby_joined", "table_joined", "table_state")

    def __init__(self, advisor, send, num_hands, log_f=None, hh_f=None, prewarm=False):
        self.advisor = advisor
        self.send = send
        self.num_hands = num_hands
        self.log_f = log_f
        self.hh_f = hh_f
        self.prewarm = prewarm  # --search-prewarm：街起点预热（缺省关 = 请求流 byte-equal）
        self.counters = {"hands": 0, "decisions": 0, "blueprint": 0, "search": 0,
                         "limp_heuristic": 0, "fallback": 0, "net_chips": 0,
                         "prewarms": 0,
                         "hh_hands": 0, "hh_skipped": 0, "watchdog_rejoins": 0}
        # 兜底/giveup 原因直方图（监控长跑异常点）：key = source 前 80 字符（同类归并、类别区分），
        # 首次见某 key 时打 [new-fallback] 提醒——新失败模式（如 Call 塌缩）立刻冒泡、不必事后 grep。
        self.fallback_reasons = {}
        self.state = {"hand": None, "table_id": None, "last_seq": 0}
        self.client_action_id = [0]
        # 看门狗（run_real 的后台线程读写）：最近一次 hand_result / 任意消息的时刻 + 当前 ws。
        self.last_hand_ts = time.time()
        self._ws = None
        self._seen_types = set()
        # 两人桌（HU）退出请求：_handle_hand_result 检测到一手仅 2 座发牌时置位 + 关 ws，
        # run_real 据此走「等 10 分钟再重连」而非「3s 断线重连」（每次连接前重置）。
        self.hu_detected = False
        # 本次连接是否打到过非两人桌手（≥3 座发牌）：run_real 据此把两人桌**连续**重试计数
        # 清零——重连后只要落到人多的桌就不再累积，只有连续都是两人桌才会逼近上限（每次连接前重置）。
        self.non_hu_hand_seen = False

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
                self._maybe_prewarm()
        elif t == "table_state":
            # 短桌占座推断（幻影座映射）：stream:state 全量快照的 seats[].in_hand 是发牌的
            # 权威信号（your_turn.players 含等局玩家，不可用）。已弃牌者会翻 false → 并集
            # 累计整手；hand_id 必须对上当前手（手间快照不算）。
            hand = self.state["hand"]
            seats = msg.get("seats")
            if hand is not None and isinstance(seats, list) and msg.get("hand_id") == hand.hand_id:
                for s in seats:
                    if s.get("in_hand") and isinstance(s.get("seat"), int) \
                            and 0 <= s["seat"] < NUM_SEATS:
                        hand.in_hand_seen.add(s["seat"])
                hand.dealt_confirmed = True
        elif t == "your_turn":
            self._handle_your_turn(ws, msg)
        elif t == "hand_result":
            self._handle_hand_result(ws, msg)
            self.counters["hands"] += 1
            # 长跑心跳（每 25 手）：tail -f 可见进度 + 兜底率漂移 + top-3 兜底原因，不必等收尾。
            if self.counters["hands"] % 25 == 0 and self.counters["hands"] < self.num_hands:
                c = self.counters
                top = sorted(self.fallback_reasons.items(), key=lambda kv: -kv[1])[:3]
                top_s = "; ".join(f"{v}× {k}" for k, v in top) or "—"
                print(f"  [♥ {c['hands']}h] dec={c['decisions']} bp={c['blueprint']} "
                      f"search={c['search']} fb={c['fallback']}"
                      f"({100.0 * c['fallback'] / c['decisions'] if c['decisions'] else 0:.1f}%) "
                      f"net={c['net_chips']} | top-fb: {top_s}", file=sys.stderr)
            if self.counters["hands"] >= self.num_hands:
                print(f"  打满 {self.num_hands} 手，离场。", file=sys.stderr)
                ws.close()

    def _maybe_prewarm(self):
        """街起点预热（--search-prewarm）：板发出、hero 行动**前**让 advisor 后台把该街
        solve 提前算进缓存（RoundStart 下 solve 全部输入在街起点已知）。只在 hero 本街还
        可能有决策时发（已 fold / all_in 跳过——粗判，advisor 侧 hero_not_active 再兜）；
        发错无害：advisor skip / key miss 时决策现解，正确性不受影响。"""
        if not self.prewarm:
            return
        hand = self.state["hand"]
        if hand is None or not hand.hole or len(hand.board or []) < 3:
            return
        for a in hand.actions:
            if a.get("seat") == hand.my_seat and a.get("action") in ("fold", "all_in"):
                return
        valid_stub = {"can_check": False, "can_call": False, "can_raise": False,
                      "min_raise": None, "max_raise": None}
        req = hand.build_request(valid_stub)
        try:
            self.advisor.prewarm(req)
            self.counters["prewarms"] += 1
        except Exception as e:
            print(f"  [prewarm 异常] {e}（忽略：决策时现解）", file=sys.stderr)

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
        # search:unanchored，缺口②续）；limp_heuristic=preflop open-limp 池启发式矩阵；
        # fallback=兜底（blueprint 结构性 fallback:* + 搜索解不出来 search_giveup:* 都算「兜底」
        # §4.1 护栏）。注意顺序：search_giveup 也以 "search" 开头，先判兜底。
        src = str(resp.get("source", ""))
        if src.startswith("fallback") or src.startswith("search_giveup"):
            self.counters["fallback"] += 1
            key = src[:80]
            if key not in self.fallback_reasons:
                print(f"  [new-fallback decisions={self.counters['decisions']}] {src}",
                      file=sys.stderr)
            self.fallback_reasons[key] = self.fallback_reasons.get(key, 0) + 1
        elif src.startswith("search"):
            self.counters["search"] += 1
        elif src.startswith("limp_heuristic"):
            self.counters["limp_heuristic"] += 1
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
        final = msg.get("final_stacks", {})
        # 两人桌（HU）检测：final_stacks 键集 = 本手发牌座（盲注跳空座，exec §3.2）→ 恰 2 座
        # = 两人桌。本实现 postflop 不支持 HU → 主动离场、关连接，由 run_real 等 10 分钟后重连
        # （连续最多 5 次）。在码深漂移 rejoin 之前判：HU 要的是断开等待、不是就地换座（同桌还是两人）。
        if isinstance(final, dict) and len(final) == 2:
            print(f"  [两人桌] 本手仅 2 座发牌（final_stacks 键={sorted(final)}）"
                  f"→ 离场，等待后重试", file=sys.stderr)
            self.hu_detected = True
            self.send(ws, {"type": "leave_table"})
            self.state["hand"] = None
            ws.close()
            return
        # ≥3 座发牌 = 非两人桌 → 标记本次连接落到了非 HU 桌（run_real 据此把两人桌**连续**
        # 重试计数清零：重连成功换到人多的桌就不再累积，只有连续 5 次仍是两人桌才放弃）。
        if isinstance(final, dict) and len(final) >= 3:
            self.non_hu_hand_seen = True
        # §4 码深漂移：我方栈漂出 [80,125]BB → leave_table + rejoin 取 2000。
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


def run_real(advisor, api_key, num_hands, action_log=None, hh_log=None, prewarm=False):
    import threading

    import websocket  # 延迟 import：离线 selftest 不需要 websocket-client

    log_f = open(action_log, "w") if action_log else None
    # HH 用 append：贯穿全程的后台采集（§4.2），跨重连 / 多次运行累积同一个文件。
    hh_f = open(hh_log, "a") if hh_log else None
    session = Session(advisor, send=_ws_send, num_hands=num_hands, log_f=log_f, hh_f=hh_f,
                      prewarm=prewarm)

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
    # 连接循环：分两类「重连」，计数独立——
    #   · 断线（run_forever 异常返回）：3s 后重连，最多 10 次（§1：断线 120s 内 resync 简化为重连）。
    #   · 两人桌（session.hu_detected）：本实现 postflop 不支持 HU → 主动离场，等 HU_RETRY_WAIT_S
    #     （10 分钟）后重连，期望换到人更多的桌；**连续** HU_MAX_RETRIES 次仍是两人桌才放弃。
    #     hu_retries 计的是「连续」次数：本次连接只要打到过非两人桌手（non_hu_hand_seen）就清零
    #     ——重连成功换到人多的桌即视为恢复，不累积。
    reconnect_attempts = 0
    hu_retries = 0
    while session.counters["hands"] < num_hands:
        session.hu_detected = False
        session.non_hu_hand_seen = False
        ws = websocket.WebSocketApp(
            WS_URL, header=header,
            on_message=on_message, on_error=on_error, on_close=on_close,
        )
        ws.run_forever(ping_interval=30, ping_timeout=10)
        # 断开后清掉 ws 引用：等待 / 重连期间看门狗读到 None 即 no-op（不往死 ws 发 rejoin）。
        session._ws = None
        if session.counters["hands"] >= num_hands:
            break
        # 恢复：本次连接打到过非两人桌手（≥3 座）→ 之前累积的两人桌**连续**重试计数清零。
        # 放在 HU/断线分支之前——无论本次连接以 HU 还是断线收尾，只要中途恢复过都算连续中断。
        if session.non_hu_hand_seen:
            hu_retries = 0
        if session.hu_detected:
            hu_retries += 1
            if hu_retries > HU_MAX_RETRIES:
                print(f"  两人桌已连续重试 {HU_MAX_RETRIES} 次仍是两人桌，放弃。", file=sys.stderr)
                break
            print(f"  [两人桌] 离场，{HU_RETRY_WAIT_S // 60} 分钟后重试"
                  f"（连续第 {hu_retries}/{HU_MAX_RETRIES} 次）…", file=sys.stderr)
            time.sleep(HU_RETRY_WAIT_S)
            reconnect_attempts = 0  # 主动等待重连不计入断线重连上限
            continue
        reconnect_attempts += 1
        if reconnect_attempts >= 10:
            print(f"  断线重连达上限（{reconnect_attempts}），放弃。", file=sys.stderr)
            break
        print(f"  断线，3s 后重连（attempt {reconnect_attempts}）…", file=sys.stderr)
        time.sleep(3)

    stop_watchdog.set()
    if log_f:
        log_f.close()
    if hh_f:
        hh_f.close()
    _report(session.counters, session.fallback_reasons)


def _street_name(s):
    if isinstance(s, str):
        return s
    # 数字街（0/1/2/3）→ 名称。
    return ["preflop", "flop", "turn", "river"][s] if isinstance(s, int) and 0 <= s <= 3 else "preflop"


def _report(counters, fallback_reasons=None):
    d = counters["decisions"]
    fb = counters["fallback"]
    print(f"hands={counters['hands']} decisions={d} "
          f"blueprint={counters['blueprint']} search={counters.get('search', 0)} "
          f"limp_heuristic={counters.get('limp_heuristic', 0)} fallback={fb} "
          f"({100.0 * fb / d if d else 0:.1f}% 兜底) "
          f"prewarms={counters.get('prewarms', 0)} "
          f"hh={counters.get('hh_hands', 0)}(+{counters.get('hh_skipped', 0)} skipped) "
          f"watchdog_rejoins={counters.get('watchdog_rejoins', 0)}",
          file=sys.stderr)
    # 兜底/giveup 原因直方图（降序）：长跑收尾一眼看清白丢手的成因构成（新失败模式 / 粒度税 / 短桌等）。
    if fallback_reasons:
        print("  兜底/giveup 原因（降序）:", file=sys.stderr)
        for k, v in sorted(fallback_reasons.items(), key=lambda kv: -kv[1]):
            print(f"    {v:>5}× {k}", file=sys.stderr)


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

    # 场景 6（短桌幻影座映射）：table_state.in_hand 占座推断 → dealt_seats / placeholder 栈 /
    # 盲注座重算 / HH dealt_est。
    _selftest_short_handed()

    # 场景 7（RoundStart 预热）：街起点（hero=BB 行动前）发 prewarm（不等响应）→ hero 决策
    # decide 先 drain 预热响应再读决策响应——验 IPC 锁步对齐不串行 + 动作仍合法。
    # search off 时 advisor 回 prewarm:skip:search_off，机制同样走通（drain 只数行数）。
    # hole 取 BB 高频 check-back 的中弱手（QQ/AK 这类 BB 几乎必加注 → 该线 reach≈0 → range 加权
    # 采样抽不到其桶 →「当前桶未被访问」giveup，看不到命中；9d8d 在 check 线 reach 高）。
    hand7 = HandState("h7", button_seat=0, my_seat=2)  # BB：flop 首行动者是 SB → 预热在 hero 行动前
    hand7.hole = ["9d", "8d"]
    for s in [3, 4, 5, 0]:
        hand7.on_player_action(s, "fold", None)
    hand7.on_player_action(1, "call", 20)   # SB complete
    hand7.on_player_action(2, "check", None)  # BB check → flop
    hand7.on_community(["7h", "2c", "Ks"], "flop")
    valid_stub = {"can_check": False, "can_call": False, "can_raise": False,
                  "min_raise": None, "max_raise": None}
    advisor.prewarm(hand7.build_request(valid_stub))
    if advisor._pending_prewarms != 1:
        raise RuntimeError(f"[prewarm] 发出后应有 1 条待 drain 响应，得 {advisor._pending_prewarms}")
    hand7.on_player_action(1, "check", None)  # SB check → 轮到 hero(BB)
    req7 = hand7.build_request(valid_flop)
    resp7 = advisor.decide(req7)
    if advisor._pending_prewarms != 0:
        raise RuntimeError(f"[prewarm] decide 后待 drain 应清零，得 {advisor._pending_prewarms}")
    _assert_legal(resp7, valid_flop, "prewarm+decision")
    print(f"[selftest 7 prewarm] resp={resp7} "
          f"(--search 开则预热入缓存、决策命中；off 则 skip——两者 IPC 锁步都须对齐)", file=sys.stderr)

    # 场景 8（两人桌检测）：hand_result 的 final_stacks 键集 = 发牌座 → 恰 2 座 = 两人桌
    # → hu_detected 置位 + 发 leave_table + 关 ws；≥3 座（含全员 fold 的 6-max）不触发。
    _selftest_hu_detection()

    print("OK: 8 个 canned 场景跑通：advisor 全程出合法动作、driver 组请求/动作包（含真栈 stacks"
          " + 短桌 dealt_seats）正常、HH 日志 byte-equal 隔离成立、prewarm IPC 锁步对齐、"
          "两人桌检测触发离场。", file=sys.stderr)


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
    # 满桌（无 table_state 证实 / 6 家全发）请求**不带** dealt_seats（旧请求 byte-equal）：
    if any('"dealt_seats"' in req for req, _ in t_on):
        raise RuntimeError("[hh] 满桌请求不应带 dealt_seats")
    print("[selftest 5 HH byte-equal] 挂/不挂 --hh-log：advisor 请求/响应 + action 包逐字节一致；"
          "HH 1 行字段齐（12 动作 + names + stacks_start + 摊牌原样）。", file=sys.stderr)


def _selftest_short_handed():
    """场景 6（短桌幻影座映射，exec §3.2）：canned 4 人短桌手走 Session 真实消息路径——
    table_state.seats[].in_hand 推断占座（in_hand=false 的等局玩家必须排除）、请求带
    dealt_seats + 非发牌座 placeholder 栈、投入按 dealt ring 真实盲注座重算（button=2 →
    真实 SB/BB = 5/0，满桌假设的 3/4 恰好都是非发牌座 = 重算分歧最大例）、HH 落 dealt_est。"""
    import os
    import tempfile

    # button=2，发牌 {0,1,2,5}（环序 2→5→0→1：SB=5、BB=0、UTG=1），我=BB(0)。座 3 是
    # 等下一手的玩家（in_hand=false、栈 777 = 不许被读）；座 4 真空。op5 起始栈 1500（非对称，
    # 钉 placeholder 与真栈不混）。preflop：UTG(1) fold → BTN(2) fold → SB(5) call → 我(BB)。
    msgs = [
        {"type": "hand_start", "hand_id": "sh-1", "seat": 0, "dealer_seat": 2,
         "table_id": "tbl-sh", "blinds": {"small_blind": 10, "big_blind": 20}},
        {"type": "hole_cards", "cards": ["Ah", "Kd"]},
        {"type": "table_state", "hand_id": "sh-1", "street": "preflop",
         "seats": [
             {"seat": 0, "name": "me", "stack": 1980, "in_hand": True, "status": "active"},
             {"seat": 1, "name": "bot1", "stack": 2000, "in_hand": True, "status": "active"},
             {"seat": 2, "name": "bot2", "stack": 2000, "in_hand": True, "status": "active"},
             {"seat": 3, "name": "waiter", "stack": 777, "in_hand": False, "status": "waiting"},
             {"seat": 5, "name": "bot5", "stack": 1490, "in_hand": True, "status": "active"},
         ]},
        {"type": "player_action", "seat": 1, "action": "fold", "amount": None,
         "street": "preflop", "stack": 2000, "contribution_delta": 0},
        {"type": "player_action", "seat": 2, "action": "fold", "amount": None,
         "street": "preflop", "stack": 2000, "contribution_delta": 0},
        {"type": "player_action", "seat": 5, "action": "call", "amount": 20,
         "street": "preflop", "stack": 1480, "contribution_delta": 10},
        {"type": "your_turn", "hand_id": "sh-1", "turn_token": "tt-sh", "seat": 0,
         "players": [{"seat": 0, "name": "me", "stack": 1980},
                     {"seat": 1, "name": "bot1", "stack": 2000},
                     {"seat": 2, "name": "bot2", "stack": 2000},
                     {"seat": 3, "name": "waiter", "stack": 777},
                     {"seat": 5, "name": "bot5", "stack": 1480}],
         "valid_actions": [{"action": "check"}, {"action": "raise", "min": 40, "max": 1980}],
         "min_raise": 40, "max_raise": 1980},
        {"type": "hand_result", "pot": 40,
         "winners": [{"seat": 0, "stack": 2020, "amount": 20, "hand_description": "x"}],
         "final_stacks": {"0": 2020, "1": 2000, "2": 2000, "5": 1480},
         "shown_cards": None},
    ]
    stub = _StubAdvisor()
    with tempfile.TemporaryDirectory() as d:
        hh_path = os.path.join(d, "hh.jsonl")
        hh_f = open(hh_path, "a")
        session = Session(stub, send=lambda ws, obj: None, num_hands=10 ** 9,
                          log_f=None, hh_f=hh_f)
        ws = _FakeWs()
        for m in msgs:
            session.handle_message(ws, m)
        hh_f.close()
        with open(hh_path) as f:
            lines = [json.loads(line) for line in f]

    req = json.loads(stub.transcript[0][0])
    if req.get("dealt_seats") != [0, 1, 2, 5]:
        raise RuntimeError(f"[short] dealt_seats 应 [0,1,2,5]（等局玩家 3 必须排除），"
                           f"得 {req.get('dealt_seats')}")
    # 真栈回推：SB=5 投 20（盲 10+call 10）→ 1480+20=1500；BB=0 投 20 → 1980+20=2000；
    # 非发牌座 3/4 = BUY_IN placeholder（777 不许泄进来）。满桌假设（盲注 seed 在 3/4）会把
    # 座 0 错算成 1980 —— 此断言钉死盲注座重算。
    if req.get("stacks") != [2000, 2000, 2000, BUY_IN, BUY_IN, 1500]:
        raise RuntimeError(f"[short] 短桌真栈回推错: {req.get('stacks')}")
    if len(lines) != 1 or lines[0].get("dealt_est") != [0, 1, 2, 5]:
        raise RuntimeError(f"[short] HH 应落 dealt_est=[0,1,2,5]，得 {lines[:1]}")
    ct = lines[0]["committed_total"]
    if ct.get("5") != 20 or ct.get("0") != 20 or ct.get("3", 0) != 0:
        raise RuntimeError(f"[short] 短桌 committed_total 应按真实盲注座（5/0）重算: {ct}")
    print("[selftest 6 短桌幻影座] table_state.in_hand 占座推断（排除等局玩家）→ dealt_seats + "
          "placeholder 栈 + 盲注座重算 + HH dealt_est 全对。", file=sys.stderr)


def _selftest_hu_detection():
    """场景 8（两人桌检测）：final_stacks 键集 = 本手发牌座 → 恰 2 座 = 两人桌 → hu_detected
    置位 + 发 leave_table + 关 ws；≥3 座（含 6-max 全员 fold = 6 键 / 3 座短桌）不触发。
    final_stacks 键集 = 发牌座（非「见 flop 人数」）：6-max 一手 4 人 preflop fold 仍是 6 键，
    故只有真两人桌（2 键）才触发，正常 6-max 不误伤。"""
    def run_hand(final_stacks):
        stub = _StubAdvisor()
        sent = []
        closed = [False]

        class _Ws:
            def close(self):
                closed[0] = True

        session = Session(stub, send=lambda ws, obj: sent.append(obj),
                          num_hands=10 ** 9, log_f=None, hh_f=None)
        ws = _Ws()
        session.handle_message(ws, {"type": "hand_start", "hand_id": "hu-1", "seat": 0,
                                    "dealer_seat": 0,
                                    "blinds": {"small_blind": 10, "big_blind": 20}})
        session.handle_message(ws, {"type": "hand_result", "pot": 30,
                                    "final_stacks": final_stacks,
                                    "winners": [], "shown_cards": None})
        return session, sent, closed[0]

    # 两人桌：2 座发牌 → 置位 + leave_table + close；non_hu_hand_seen 保持 False（未恢复）。
    s2, sent2, closed2 = run_hand({"0": 1990, "1": 2010})
    if not s2.hu_detected:
        raise RuntimeError("[hu] 2 座发牌应置 hu_detected")
    if {"type": "leave_table"} not in sent2:
        raise RuntimeError(f"[hu] 两人桌应发 leave_table，得 {sent2}")
    if not closed2:
        raise RuntimeError("[hu] 两人桌应关闭 ws")
    if s2.non_hu_hand_seen:
        raise RuntimeError("[hu] 两人桌手不应置 non_hu_hand_seen（无恢复 → 连续计数不清零）")
    # 6-max 全员发牌（即便多数 preflop fold）：final_stacks 6 键 → 不触发 + 标记恢复。
    s6, sent6, closed6 = run_hand({str(i): 2000 for i in range(NUM_SEATS)})
    if s6.hu_detected or closed6 or any(o.get("type") == "leave_table" for o in sent6):
        raise RuntimeError("[hu] 6 座发牌（全员 fold 也是 6 键）不应触发两人桌退出")
    if not s6.non_hu_hand_seen:
        raise RuntimeError("[hu] ≥3 座发牌应置 non_hu_hand_seen（恢复 → run_real 清零连续计数）")
    # 3 座短桌：不触发 + 标记恢复（≥3 座即非两人桌）。
    s3, _, closed3 = run_hand({"0": 2000, "1": 2000, "2": 2000})
    if s3.hu_detected or closed3:
        raise RuntimeError("[hu] 3 座短桌不应触发两人桌退出")
    if not s3.non_hu_hand_seen:
        raise RuntimeError("[hu] 3 座短桌应置 non_hu_hand_seen（恢复信号）")
    print("[selftest 8 两人桌检测] final_stacks 2 键 → hu_detected + leave_table + close（不标恢复）；"
          "≥3 键（6-max / 短桌）不触发 + 标 non_hu_hand_seen（连续计数清零信号）。", file=sys.stderr)


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
    # range 先验平滑 λ（advisor 默认开 0.25；显式 0 = 关，A/B 对照臂用）。
    p.add_argument("--search-range-uniform-mix", type=float, default=None)
    # 脱锚搜索档一前缀 reach（advisor 默认 on；off = A/B 对照臂 / 回退到 uniform 先验）。
    # 不传 = 不透传，吃 advisor 默认（ON）。
    p.add_argument("--search-unanchored-prefix-reach", choices=["on", "off"], default=None)
    # 深码 SPR 自适应菜单（deep_menu_for：深 {1pot} / 浅 ≤3-way {0.5,1} 全层级）。
    p.add_argument("--search-deep-menu", action="store_true")
    # 子树独立桶表（如 500/500/500；blueprint 仍用 --bucket-table 的表）。
    p.add_argument("--search-bucket-table", default=None)
    # solve update 并行线程数（同预算 update ≈ ×核数；只助 solve 侧，建树仍单线程）。
    p.add_argument("--search-solve-threads", type=int, default=None)
    # RoundStart 预热：街起点（hero 行动前）让 advisor 提前 build+solve 暖缓存，
    # wall 藏进对手行动时间。driver 侧行为（advisor 按请求响应，无需自己的 flag）。
    p.add_argument("--search-prewarm", action="store_true")
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
        if args.search_range_uniform_mix is not None:
            extra += ["--search-range-uniform-mix", str(args.search_range_uniform_mix)]
        if args.search_unanchored_prefix_reach is not None:
            extra += ["--search-unanchored-prefix-reach", args.search_unanchored_prefix_reach]
        if args.search_deep_menu:
            extra.append("--search-deep-menu")
        if args.search_bucket_table:
            extra += ["--search-bucket-table", args.search_bucket_table]
        if args.search_solve_threads is not None:
            extra += ["--search-solve-threads", str(args.search_solve_threads)]
    if args.search_prewarm and not args.search:
        raise SystemExit("--search-prewarm 需配 --search（拒绝静默：没有搜索就没有可预热的 solve）")
    if args.search_unanchored_prefix_reach is not None and not args.search:
        raise SystemExit("--search-unanchored-prefix-reach 需配 --search"
                         "（拒绝静默：没有搜索就没有脱锚 range 先验，否则误以为在跑 off 臂实则纯 blueprint）")
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
                     hh_log=args.hh_log if args.hh_log else None,
                     prewarm=args.search_prewarm)
    finally:
        advisor.close()


if __name__ == "__main__":
    main()

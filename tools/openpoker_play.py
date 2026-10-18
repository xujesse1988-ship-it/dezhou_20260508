#!/usr/bin/env python3
"""OpenPoker N-max WS driver（默认 6 座；9 座桌传 --seats 9 + 9 座 blueprint）
（docs/temp/openpoker_client_design_2026_06_02.md §2/§3/§7）。

网络 / token / 重连 / 限速全在本文件（Python，websocket-client），策略全在常驻 Rust
advisor（tools/openpoker_advisor.rs）—— Rust crate 零网络依赖（invariant）。

driver 职责（§3）：
  - WS 连 wss://openpoker.ai/ws + Bearer 鉴权；join_lobby{buy_in:2000}（锁 100BB）。
  - 累计每手 betting 历史（hand_start/hole_cards/player_action/community_cards）→ 组 advisor 请求。
  - your_turn → 调 advisor 拿 {action, amount} → 回 action{turn_token, client_action_id}。
  - §4 码深漂移：每手后我方栈漂出 [80,180]BB → leave_table + rejoin 取 2000（控我方栈）。
  - §4 两人桌：一手仅 2 座发牌（HU）→ 本实现 postflop 不支持 → leave_table + 离线等 10 分钟
    重连，**连续** 5 次仍是两人桌才放弃（中途换到人多的桌即清零；HU 是 live 最大出血点，exec §3.2）。
  - 断线重连：仍在座（already_seated）→ resync_request 补事件 + 快照 token 续打，不离桌；
    离桌换座等自己的 player_left 再 join；关桌等服务器重排（Session 入座状态机）。限速 20 msg/s。

HH 日志（--hh-log，realtime_search_openpoker_exec §4.2 数据管道）：每手 hand_result 落一行
**全桌手牌历史** JSONL——整手动作序 + board + 对手 name + 回推 hand-start 真栈 +
winners/final_stacks/shown_cards 原样，喂 Rust 侧 `openpoker_hh_aivat`
（openpoker_hh::hh_to_multiway_input → 多人 AIVAT，mbb/g）。**advisor 路径隔离**：HH 只读
已累计状态、绝不动 build_request 消费的字段；挂/不挂 --hh-log advisor 请求/输出 byte-equal
由 --selftest 的 canned 序列钉死（场景 5）。

WS 日志（--ws-log，缺省关）：与服务端往来的原始 WS 消息逐条落 JSONL（收 / 发原文 + 连接
open/close/error 事件，带连接序号），复盘服务端实际推送用。同样纯旁路：挂/不挂 advisor 请求/输出 +
线上发出的字节 byte-equal（场景 5b）。

调试（--debug-log）：开后把每手中间数据打到 stderr——手起（button/座）、发牌、对手动作、决策点
（请求摘要含真栈/幻影座 + advisor 响应全诊断字段 source/probs/info_set/solve_updates/chosen +
实发动作包）、结果（winners/final_stacks/我方栈）。**纯展示、只读已累计状态**，不动任何发往
advisor / ws 的字节 → IPC + 出动作 byte-equal 不受影响（与 --hh-log 同隔离原则）。

座位数（--seats，默认 6）：= OpenPoker 桌面座位数 = advisor solver 座位数（透传 advisor
`--seats`，blueprint 须用同座位数训练；advisor 启动时 layout 指纹不符直接报错退出）。driver 启动后
核对 advisor ready 行的 n_seats，不一致即拒绝开打。缺省 6 时 advisor 命令行 / 请求流与旧版逐字节相同。

blueprint 二选一：--checkpoint（完整 dense ckpt）或 --cropped-checkpoint（按街裁切的 crop 文件，
advisor 要求同时开 --search --search-trigger all-postflop，转牌 / 河牌必走实时搜索）。

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
  手大量走 advisor 兜底（§4 已知短板，非 bug）；买入锁 2000 + 漂出 [80,180]BB leave/rejoin 只控我方栈。
"""

import argparse
import json
import os
import sys
import threading
import time

from poker_live.advisor_client import (add_advisor_args, advisor_extra_args,
                                       exploit_enabled, start_advisor,
                                       validate_advisor_args)
from poker_live.decision import make_request, tally_source
from poker_live.wslog import WsLog

# OpenPoker 协议常量（§0/§1）。
WS_URL = "wss://openpoker.ai/ws"
REGISTER_URL = "https://api.openpoker.ai/api/register"
BUY_IN = 2000           # 锁 100BB（§4 码深漂移缓解①）。
BIG_BLIND_OP = 20       # OpenPoker 默认 10/20。
SMALL_BLIND_OP = 10
STACK_LEAVE_LO = 80 * BIG_BLIND_OP   # 1600：我方栈 < 80BB → leave/rejoin。
STACK_LEAVE_HI = 180 * BIG_BLIND_OP  # 3600：我方栈 > 180BB → leave/rejoin。
# 桌面座位数（= advisor solver 座位数）。main() 按 --seats 改写（缺省 6 = 旧行为）；本模块所有
# 函数在运行期读它（无定义期捕获），selftest canned 场景按 6 座写，只在 6 座下跑。
NUM_SEATS = 6
_BOARD_LEN = {"preflop": 0, "flop": 3, "turn": 4, "river": 5}

# 两人桌（HU）处理：本实现 postflop 不支持两人（HU 是 live 最大出血点，exec §3.2 ——
# 6-max 树表达不了跨街反转、smoke 实测 87.5% 兜底）。检测到一手仅 2 座发牌 → 主动离场，
# 等 HU_RETRY_WAIT_S 后重连（期望换到人更多的桌）；**连续** HU_MAX_RETRIES 次仍是两人桌才放弃
# （重连后只要落到人多的桌就清零，不累积）。
LEAVE_TIMEOUT_S = 180   # 发 leave_table 后这么久没等到自己的 player_left → 当作已离桌（一手可能打 2 分钟）
RESYNC_WAIT_S = 3       # 等 resync_response 的上限；超时按现有历史对暂存的 your_turn 决策
HU_RETRY_WAIT_S = 120   # 2 分钟（2026-06-21 由 10 分钟下调：lobby HU 多时更快换桌找多人桌）。
HU_MAX_RETRIES = 30     # 配合 2 分钟重试：~60min 才放弃，避免频繁 give-up 触发昂贵的 10B blueprint 重载。

# HH 日志（§4.2）：player_action 原始字段子集，平行落进 actions_ext（advisor 不读）。
# contribution_delta / stack_before / stack_after 是 live 校准发现的字段——比 driver 自跟
# committed 更稳（all_in 无 amount 时 committed_total 有已知缺口），Rust 解析侧留作修复材料。
_EXT_KEYS = ("amount", "amount_mode", "street", "stack", "stack_before", "stack_after",
             "contribution_delta", "to_call_before")


# ===========================================================================
# 单手状态累计（§3：OpenPoker 有状态推送 → driver 自己累计成 advisor 要的历史）
# ===========================================================================
def _commit_step(committed, seat, a, amount, all_in_delta=None):
    """单步本街 committed 转移（on_player_action 增量路径与短桌投入重算共用，保两边不漂）。
    返回 (to, delta)：to = raise/bet 的本街累计到额；delta = 本动作投入增量。
    all_in_delta = 不带 amount 的 all_in 的投入增量（= 该座全下前的剩余栈，见
    HandState.on_player_action）；None = 没有这个信息（旧行为：增量 0）。"""
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
        elif all_in_delta is not None:
            committed[seat] = prev + all_in_delta
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
        # 叠加剥削（--exploit）：与 actions 平行的**驱动侧街**（on_player_action 时的 self.street）。
        # 用驱动侧跟踪的街而非 actions_ext.street（后者关闭轮错位一街，memory
        # reference_openpoker_hh_jsonl_parsing）。observe_record 据它给 advisor Profiler 逐动作标街。
        self._action_streets = []
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
        ext = player_action 原始字段子集（HH 日志用，advisor 不读）。
        返回值：不带 amount 的 all_in 用 ext.stack_before 补出投入时 = True（调用方须把该座
        剩余栈记为 0，服务端这条消息的 stack 字段还是全下前的值）；其余 False。"""
        a = (action or "").lower()
        # 不带 amount 的 all_in：服务端这条消息的 stack / stack_before / stack_after 都是全下**前**
        # 的剩余栈、contribution_delta=0（2026-10-16 run1–3 全部 5 次 all_in 实测）。全下 = 把剩余栈
        # 全部投入 → 增量 = stack_before。修前增量记 0、之后 your_turn 又把该座栈更新成 0 →
        # hand-start 栈只剩已投入部分（run3：翻牌全下的 BB 被算成 44 → auth_seat_mismatch 放弃）。
        all_in_delta = None
        if a == "all_in" and amount is None and ext and ext.get("stack_before") is not None:
            all_in_delta = int(round(ext["stack_before"]))
        to, delta = _commit_step(self.committed, seat, a, amount, all_in_delta)
        # 缺口②：本手累计投入 += 本动作增量（本街新 committed − 旧）。all_in 无 amount 且无
        # stack_before → 增量 0（信息缺，advisor 真栈重放会因 apply 非法回落 fold，不污染 blueprint 路径）。
        self.committed_total[seat] = self.committed_total.get(seat, 0) + delta
        self.actions.append({"seat": seat, "action": a, **({"to": to} if to is not None else {})})
        self.actions_ext.append(dict(ext) if ext else {})
        self._action_streets.append(self.street)  # 剥削 observe：逐动作驱动侧街（与 actions 平行）。
        if seat is not None:
            self.acted.add(seat)
        self._raw_events.append(("act", seat, a, amount, all_in_delta))
        return all_in_delta is not None

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
            _, seat, a, amount, all_in_delta = ev
            _, delta = _commit_step(committed, seat, a, amount, all_in_delta)
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

    def build_request(self, valid, include_names=False):
        """组 advisor 请求（openpoker_advisor::Request）。`stacks` 仅在真栈已知时附带
        （缺口②实时搜索读；缺省 → advisor 退对称 100BB blueprint，byte-equal）。
        `dealt_seats` 仅短桌且占座可判时附带（幻影座映射；满桌不带 = 请求 byte-equal 旧行为）。
        `include_names`（--exploit 开）：附 `names`（seat→name）供 advisor 脱锚搜索按名查画像；
        缺省关 = 不附 = 旧请求 byte-equal。"""
        dealt = self.dealt_now()
        return make_request(
            self.hole, self.board, self.button_seat, self.my_seat, NUM_SEATS,
            SMALL_BLIND_OP, BIG_BLIND_OP, self.actions, valid,
            stacks=self.hand_start_stacks(),
            dealt_seats=sorted(dealt) if dealt is not None and len(dealt) < NUM_SEATS else None,
            names=self.names if include_names else None)

    def observe_record(self):
        """叠加剥削（--exploit）：一手完整观测给 advisor Profiler（`{observe,names,actions}`）。
        `actions` = 逐动作 `{seat, street, action}`（街用驱动侧 `_action_streets`，避开
        actions_ext.street 错位坑）。driver 在我方弃牌后仍续记牌桌动作到 hand_result → 覆盖整手所有
        座位（含我退出后的街），对手统计无「只看我参与的线」偏差。"""
        acts = [
            {"seat": a["seat"], "street": st, "action": a["action"]}
            for a, st in zip(self.actions, self._action_streets)
        ]
        return {"observe": 1, "names": self.names, "actions": acts}

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
                    "lobby_joined", "table_joined", "table_state",
                    "player_left", "table_closed", "resync_response")

    def __init__(self, advisor, send, num_hands, log_f=None, hh_f=None, prewarm=False,
                 exploit=False, debug=False):
        self.advisor = advisor
        self.send = send
        self.num_hands = num_hands
        self.log_f = log_f
        self.hh_f = hh_f
        self.prewarm = prewarm  # --search-prewarm：街起点预热（缺省关 = 请求流 byte-equal）
        self.exploit = exploit  # --exploit：决策请求带 names + 每手结束发 observe（缺省关 = byte-equal）
        # --debug-log：把牌局中间数据（手起/发牌/动作/决策请求+响应/结果）打到 stderr。**纯展示**：
        # 只读已累计状态，绝不改 advisor 请求 / 动作包 → IPC + 出动作 byte-equal 不受影响（缺省关）。
        self.debug = debug
        self.counters = {"hands": 0, "decisions": 0, "blueprint": 0, "search": 0,
                         "limp_heuristic": 0, "fallback": 0, "net_chips": 0,
                         "prewarms": 0,
                         "hh_hands": 0, "hh_skipped": 0, "watchdog_rejoins": 0,
                         "resyncs": 0, "dup_events": 0}
        # 兜底/giveup 原因直方图（监控长跑异常点）：key = source 前 80 字符（同类归并、类别区分），
        # 首次见某 key 时打 [new-fallback] 提醒——新失败模式（如 Call 塌缩）立刻冒泡、不必事后 grep。
        self.fallback_reasons = {}
        # last_seq = 当前桌已处理的最大**事件** table_seq（换桌清零）：同桌 table_seq 严格递增（run5
        # 实测 3.8 万条无重复 / 回退）→ ≤ last_seq 的事件是重连 / resync 补发的重复，丢弃。
        self.state = {"hand": None, "table_id": None, "last_seq": 0}
        self.client_action_id = [0]
        # 入座状态机（docs.openpoker.ai bot-lifecycle；run5 2026-10-17 实测重连 / 换桌问题后重写）：
        #   none → join_lobby → joining →（lobby_joined / table_joined）→ seated
        #   seated → leave_table → leaving →（自己的 player_left / not_at_table）→ none → join_lobby
        # 每个转移只发一条消息，绝不在收到错误时回发 leave+join（旧实现在 already_seated /
        # leave_pending 上来回 3163 轮、撞限速丢动作）。table_closed 后服务器自动重排 → joining。
        self.seat_state = "none"
        self.leave_ts = 0.0
        self.my_name = None   # connected.name：认自己的 player_left
        self.my_seat = None   # 当前桌我方座位（table_joined / hand_start / already_seated）
        self.stopping = False  # Ctrl-C：run_real 不再重连
        # 重连后仍在座（already_seated）→ resync_request 补漏掉的事件 + 快照（每次连接最多一次）。
        # 等 resync_response 期间到的 your_turn 先暂存，补完历史再决策（超时 RESYNC_WAIT_S 直接决策）。
        self.resync_sent = False
        self.resync_pending_ts = None
        self.stashed_turn = None
        self.acted_tokens = set()  # 已出过牌的 turn_token：快照与实时 your_turn 同 token 只出一次
        # 真实联机由 run_real 的 ws 线程与看门狗线程共用 → 消息处理 / 看门狗串行化。
        self.lock = threading.RLock()
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

    def _dbg(self, msg):
        """--debug-log：一行牌局中间数据到 stderr（仅 self.debug 开时）。纯展示、不改任何
        发往 advisor / ws 的字节（byte-equal 不变）。"""
        if self.debug:
            print(f"  [dbg] {msg}", file=sys.stderr)

    def join(self, ws, why):
        """none → joining：发一次 join_lobby。已在排队 / 在座 / 离桌中都不发（避免重复入队）。"""
        if self.stopping or self.seat_state != "none":
            return
        print(f"  [join] {why} → join_lobby", file=sys.stderr)
        self.seat_state = "joining"
        self.last_hand_ts = time.time()  # 排队期间没有 hand_result 是正常的，喂狗
        self.send(ws, {"type": "join_lobby", "buy_in": BUY_IN})

    def leave(self, ws, why):
        """seated → leaving：只发一次 leave_table。服务器可能回 leave_pending（本手打完才走），
        真正离桌以自己的 player_left / not_at_table 为准（_on_left），之后才 join_lobby——
        旧实现 leave + join 连发，join 抢在 leave 生效前入座、随后又被那条 leave 踢出（run5 3 次）。"""
        if self.seat_state == "leaving":
            return
        print(f"  [leave] {why} → leave_table（等 player_left 再进大厅）", file=sys.stderr)
        self.seat_state = "leaving"
        self.leave_ts = time.time()
        self.send(ws, {"type": "leave_table"})

    def rejoin(self, ws, why):
        """换一个干净座位：先离桌，确认离开后自动 join（看门狗 / 码深漂移用）。"""
        self.leave(ws, why)

    def _on_left(self, ws, why):
        self.state["hand"] = None
        self.seat_state = "none"
        self.join(ws, why)

    def watchdog_check(self, stale_after=300, leave_timeout=LEAVE_TIMEOUT_S):
        """run_real 后台线程每 30s 调一次（持 lock）。
        - 离桌超过 leave_timeout 还没等到自己的 player_left → 当作已离开，进大厅；
        - 超过 stale_after 秒没有 hand_result（正常 ~1 手/分钟，live 2026-06-11 曾被移出桌后
          永远空等）→ 在座则换座、排队中则重发一次 join、不在桌则 join。"""
        with self.lock:
            ws = self._ws
            if ws is None:
                return
            now = time.time()
            try:
                if self.seat_state == "leaving":
                    if now - self.leave_ts > leave_timeout:
                        self.counters["watchdog_rejoins"] += 1
                        self._on_left(ws, f"离桌 {leave_timeout}s 未见 player_left（watchdog）")
                elif now - self.last_hand_ts > stale_after:
                    self.counters["watchdog_rejoins"] += 1
                    self.last_hand_ts = now  # 防连环触发
                    why = f"{stale_after}s 无 hand_result（watchdog）"
                    if self.seat_state == "seated":
                        self.leave(ws, why)
                    else:
                        self.seat_state = "none"
                        self.join(ws, why)
            except Exception as e:
                print(f"  [watchdog] 发送失败: {e}", file=sys.stderr)

    def tick(self):
        """run_real 后台线程每秒调一次：resync 迟迟不回时，别让暂存的 your_turn 一直等
        （轮到我方时桌上没有别的消息，单靠消息驱动的超时检查醒不过来）。"""
        with self.lock:
            if self._ws is not None:
                self._flush_stale_resync(self._ws)

    def _flush_stale_resync(self, ws, wait=None):
        wait = RESYNC_WAIT_S if wait is None else wait
        if self.resync_pending_ts is not None and time.time() - self.resync_pending_ts > wait:
            print(f"  [resync] {wait}s 未收到 resync_response → 按现有历史继续", file=sys.stderr)
            self.resync_pending_ts = None
            self._play_stashed(ws)

    def _play_stashed(self, ws):
        turn, self.stashed_turn = self.stashed_turn, None
        if turn is not None:
            self._handle_your_turn(ws, turn)

    def _is_me(self, msg):
        if self.my_name is not None and msg.get("name") is not None:
            return msg.get("name") == self.my_name
        return msg.get("seat") is not None and msg.get("seat") == self.my_seat

    def handle_message(self, ws, msg):
        with self.lock:
            self._handle(ws, msg)

    def _handle(self, ws, msg):
        t = msg.get("type")
        self._ws = ws
        self._flush_stale_resync(ws)
        tid = msg.get("table_id")
        if tid:
            # 换桌（或首次入桌）时打观战链接（arena 页面按 table_id 路由）。
            if tid != self.state["table_id"]:
                print(f"  [table] table_id={tid} "
                      f"观战: https://openpoker.ai/zh/arena/{tid}", file=sys.stderr)
                self.state["table_id"] = tid
                self.state["last_seq"] = 0
            seq = msg.get("table_seq")
            # 只按离散事件（stream:event）记进度：table_state 快照与事件共用 table_seq 计数，重连时
            # 服务器先推的快照 seq 大于断线期间漏掉的事件——按它记会把 resync 补发的事件当重复丢掉、
            # 也会让 resync_request 声称「都收到了」。
            is_event = msg.get("stream") == "event" or (msg.get("stream") is None
                                                        and t != "table_state")
            if isinstance(seq, int) and is_event:
                if seq <= self.state["last_seq"]:
                    self.counters["dup_events"] += 1
                    return
                self.state["last_seq"] = seq
        if t not in self._seen_types:
            self._seen_types.add(t)
            if t not in self._KNOWN_TYPES:
                print(f"  [msg] 未处理消息类型 {t!r} keys={sorted(msg.keys())}", file=sys.stderr)

        if t == "connected":
            print(f"  connected agent_id={msg.get('agent_id')} name={msg.get('name')}",
                  file=sys.stderr)
            self.my_name = msg.get("name") or self.my_name
            self.resync_sent = False
            self.resync_pending_ts = None
            self.stashed_turn = None
            # 新连接一律先 join_lobby：仍在座时服务器回 already_seated（带 table_id）→ resync；
            # 冷启动（上个进程留下的座位）走同一条路。
            self.seat_state = "none"
            self.join(ws, "connected")
        elif t == "error":
            code = msg.get("code")
            print(f"  [error] {msg}", file=sys.stderr)
            if code == "auth_failed":
                ws.close()
            elif code == "already_seated":
                # 断线 120s 内服务器保留座位（docs）：不是死桌，留在原座，用 error 的 table_id
                # resync 补漏掉的事件；死桌由看门狗兜。离桌中收到（join 与 leave 赛跑）→ 等 player_left。
                if isinstance(msg.get("seat"), int):
                    self.my_seat = msg["seat"]
                if self.seat_state == "leaving":
                    return
                self.seat_state = "seated"
                if not self.resync_sent and self.state["table_id"]:
                    self.resync_sent = True
                    self.resync_pending_ts = time.time()
                    self.counters["resyncs"] += 1
                    print(f"  [resync] 仍在座 seat={self.my_seat} → resync_request "
                          f"last_table_seq={self.state['last_seq']}", file=sys.stderr)
                    self.send(ws, {"type": "resync_request", "table_id": self.state["table_id"],
                                   "last_table_seq": self.state["last_seq"]})
            elif code == "not_at_table":
                if self.resync_pending_ts is not None:
                    # resync 被拒 = 座位已经没了：暂存的 your_turn 作废，重新进大厅。
                    self.resync_pending_ts = None
                    self.stashed_turn = None
                    self._on_left(ws, "resync: not_at_table")
                elif self.seat_state == "leaving":
                    self._on_left(ws, "not_at_table")
            elif code not in ("leave_pending", "rate_limited") \
                    and self.resync_pending_ts is not None:
                # resync 期间的其他错误：不再等，按现有历史决策。
                self.resync_pending_ts = None
                self._play_stashed(ws)
        elif t in ("lobby_joined", "table_joined"):
            # 排队 / 入座流程中没有 hand_result 是正常状态——喂狗，免得看门狗在大厅队列里
            # 误触发换座（会被重排队尾、可能循环）。
            self.last_hand_ts = time.time()
            if t == "lobby_joined":
                if self.seat_state != "leaving":
                    self.seat_state = "joining"
            else:
                # 新座位：上一桌没打完的手（断线 / 关桌）作废，避免它的 hand_result 挂错手。
                self.seat_state = "seated"
                self.state["hand"] = None
                if isinstance(msg.get("seat"), int):
                    self.my_seat = msg["seat"]
            print(f"  [{t}] position={msg.get('position')} wait={msg.get('estimated_wait')} "
                  f"seat={msg.get('seat')}", file=sys.stderr)
        elif t == "table_closed":
            # 桌散：服务器把剩下的人自动重新排队（docs bot-lifecycle；run5 5 次关桌都紧跟
            # lobby_joined）→ 不发 join，等它；没排上由看门狗兜。
            print(f"  [table_closed] reason={msg.get('reason')} → 等服务器重新排队",
                  file=sys.stderr)
            self.state["hand"] = None
            self.seat_state = "joining"
            self.last_hand_ts = time.time()
        elif t == "player_left":
            if self._is_me(msg):
                print(f"  [player_left] 我方离桌 seat={msg.get('seat')} "
                      f"reason={msg.get('reason')}", file=sys.stderr)
                if self.seat_state == "joining":
                    self.state["hand"] = None  # 关桌后的 player_left：服务器已在重排
                else:
                    self._on_left(ws, f"player_left reason={msg.get('reason')}")
        elif t == "resync_response":
            self._handle_resync(ws, msg)
        elif t == "hand_start":
            self.state["hand"] = HandState(msg.get("hand_id"), msg.get("dealer_seat"),
                                           msg.get("seat"))
            if self.seat_state != "leaving":
                self.seat_state = "seated"
            if isinstance(msg.get("seat"), int):
                self.my_seat = msg["seat"]
            self._dbg(f"=== hand_start hand_id={msg.get('hand_id')} "
                      f"button={msg.get('dealer_seat')} my_seat={msg.get('seat')} ===")
        elif t == "hole_cards":
            if self.state["hand"]:
                self.state["hand"].hole = msg.get("cards")
                self._dbg(f"hole {' '.join(msg.get('cards') or [])}")
        elif t == "player_action":
            if self.state["hand"]:
                ext = {k: msg[k] for k in _EXT_KEYS if k in msg}
                all_in_resolved = self.state["hand"].on_player_action(
                    msg.get("seat"), msg.get("action"), msg.get("amount"), ext=ext)
                # 不带 amount 的 all_in：消息里的 stack 还是全下前的值 → 剩余栈记 0。
                self.state["hand"].update_stacks(
                    msg.get("seat"), 0 if all_in_resolved else msg.get("stack"))
                self._dbg(f"act seat={msg.get('seat')} {msg.get('action')}"
                          + (f" amount={msg.get('amount')}"
                             if msg.get("amount") is not None else "")
                          + (f" stack={msg.get('stack')}"
                             if msg.get("stack") is not None else ""))
        elif t == "community_cards":
            if self.state["hand"]:
                self.state["hand"].on_community(msg.get("cards", []),
                                                _street_name(msg.get("street")))
                self._maybe_prewarm()
                self._dbg(f"{_street_name(msg.get('street'))} "
                          f"board={' '.join(msg.get('cards') or [])}")
        elif t == "table_state":
            self._apply_table_state(msg)
        elif t == "your_turn":
            if self.resync_pending_ts is not None:
                # 等 resync 补完断线期间的事件再决策（同 token 快照若先出过牌，这条会被去重）。
                print("  [resync] your_turn 先到 → 暂存，等 resync_response", file=sys.stderr)
                self.stashed_turn = msg
            else:
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

    def _apply_table_state(self, msg):
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

    def _handle_resync(self, ws, msg):
        """resync_response（docs message-types）：replayed_events 按 table_seq 升序当普通消息
        重放（已见过的被 table_seq 去重），snapshot 最后装上。快照的 hero 带 turn_token +
        valid_actions = 仍轮到我方（断线不重置行动时限）→ 按原 token 出牌。"""
        self.resync_pending_ts = None
        role = msg.get("role")
        events = [e for e in (msg.get("replayed_events") or []) if isinstance(e, dict)]
        events.sort(key=lambda e: e.get("table_seq") if isinstance(e.get("table_seq"), int) else 0)
        print(f"  [resync] role={role} 补发 {len(events)} 条 "
              f"seq {msg.get('from_table_seq')}→{msg.get('to_table_seq')}", file=sys.stderr)
        if role != "player":
            self.stashed_turn = None
            self._on_left(ws, f"resync role={role}（不在座）")
            return
        tid = self.state["table_id"]
        for ev in events:
            if ev.get("type") == "your_turn":
                continue  # 行动权以快照为准，避免按过期 token 出牌
            ev = dict(ev)
            ev.setdefault("table_id", tid)
            self._handle(ws, ev)
        snap = msg.get("snapshot") or {}
        if not isinstance(snap, dict):
            snap = {}
        self._apply_table_state(snap)
        hand = self.state["hand"]
        hid = snap.get("hand_id")
        if hand is not None and hid and hand.hand_id != hid:
            # 补发也没把我们带到快照这一手（中途换手）→ 旧手作废；这一手历史不全，只能安全出牌。
            self.state["hand"] = hand = None
        hero = snap.get("hero") if isinstance(snap.get("hero"), dict) else {}
        if hand is not None and not hand.hole and hero.get("hole_cards"):
            hand.hole = hero["hole_cards"]
        if hero.get("turn_token") and hero.get("valid_actions"):
            turn = {"type": "your_turn", "hand_id": hid, "turn_token": hero["turn_token"],
                    "valid_actions": hero["valid_actions"],
                    "min_raise": snap.get("min_raise_to"), "max_raise": snap.get("max_raise_to")}
            if isinstance(snap.get("board"), list):
                turn["community_cards"] = snap["board"]
            if isinstance(snap.get("seats"), list):
                turn["players"] = [{"seat": s.get("seat"), "name": s.get("name"),
                                    "stack": s.get("stack")}
                                   for s in snap["seats"] if isinstance(s, dict)]
            print("  [resync] 快照显示轮到我方 → 按原 turn_token 出牌", file=sys.stderr)
            self._handle_your_turn(ws, turn)
        self._play_stashed(ws)

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
        token = msg.get("turn_token")
        if token and token in self.acted_tokens:
            return  # resync 快照与实时 your_turn 同一 token：只出一次（重用会被 action_rejected）
        hand = self.state["hand"]
        if hand is None or (msg.get("hand_id") and msg.get("hand_id") != hand.hand_id):
            # 没跟到这手的开头（中途入座 / 重连换了手）：历史不全不能问 advisor，但也不能
            # 不回——不回就等服务器 120s 超时。能过牌就过牌，否则弃牌。
            self._act_safe(ws, msg, "fallback:no_hand_state")
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
        req = hand.build_request(valid, include_names=self.exploit)
        try:
            resp = self.advisor.decide(req)
        except Exception as e:
            print(f"  [advisor 异常] {e} → fold 兜底", file=sys.stderr)
            resp = {"action": "fold", "source": "fallback:advisor_exception"}
        self.counters["decisions"] += 1
        # source 分桶（blueprint / search / limp_heuristic / fallback，见 tally_source）。
        tally_source(self.counters, self.fallback_reasons, resp.get("source", ""))
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
        self._mark_acted(token)
        if self.log_f:
            self.log_f.write(json.dumps({"req": req, "resp": resp, "sent": out},
                                        ensure_ascii=False) + "\n")
            self.log_f.flush()
        if self.debug:
            self._dbg_decision(hand, req, resp, out)

    def _mark_acted(self, token):
        if token:
            if len(self.acted_tokens) > 1000:
                self.acted_tokens.clear()
            self.acted_tokens.add(token)

    def _act_safe(self, ws, msg, source):
        acts = {va.get("action") for va in msg.get("valid_actions", []) or []}
        action = "check" if "check" in acts else "fold"
        self.counters["decisions"] += 1
        self.counters["fallback"] += 1
        if source not in self.fallback_reasons:
            print(f"  [new-fallback decisions={self.counters['decisions']}] {source}",
                  file=sys.stderr)
        self.fallback_reasons[source] = self.fallback_reasons.get(source, 0) + 1
        self.client_action_id[0] += 1
        out = {"type": "action", "hand_id": msg.get("hand_id"), "action": action,
               "amount": None, "client_action_id": f"jx-{self.client_action_id[0]}",
               "turn_token": msg.get("turn_token")}
        self.send(ws, out)
        self._mark_acted(msg.get("turn_token"))
        if self.log_f:
            self.log_f.write(json.dumps({"req": None, "resp": {"action": action, "source": source},
                                         "sent": out}, ensure_ascii=False) + "\n")
            self.log_f.flush()

    def _dbg_decision(self, hand, req, resp, out):
        """--debug-log：决策点中间数据——请求摘要（含真栈 / 幻影座）、advisor 响应全诊断字段
        （source / probs / info_set / solve_updates / chosen / exploit）、实发动作包。req/resp/out
        已成形，只读不改（byte-equal 不变）。"""
        v = req.get("valid", {})
        acts = ",".join(a for a, ok in (("check", v.get("can_check")),
                                        ("call", v.get("can_call")),
                                        ("raise", v.get("can_raise"))) if ok) or "—"
        rng = (f" raise=[{v.get('min_raise')},{v.get('max_raise')}]"
               if v.get("can_raise") else "")
        self._dbg(f"your_turn street={hand.street} hole={''.join(hand.hole or [])} "
                  f"board={' '.join(hand.board or [])} valid={{{acts}}}{rng}"
                  + (f" stacks={req['stacks']}" if "stacks" in req else "")
                  + (f" dealt={req['dealt_seats']}" if "dealt_seats" in req else ""))
        extra = " ".join(f"{k}={resp[k]}" for k in
                         ("info_set", "solve_updates", "chosen", "probs", "exploit")
                         if resp.get(k) is not None)
        self._dbg(f"decide action={resp.get('action')} amount={resp.get('amount')} "
                  f"source={resp.get('source')}" + (f"  {extra}" if extra else ""))
        self._dbg(f"sent action={out['action']} amount={out['amount']} "
                  f"id={out['client_action_id']}")

    def _handle_hand_result(self, ws, msg):
        self.last_hand_ts = time.time()
        hand = self.state["hand"]
        if hand is not None and msg.get("hand_id") and msg.get("hand_id") != hand.hand_id:
            # 结算的不是我们跟着的那手（断线 / 关桌后旧手没清掉，run5 3 条 HH 张冠李戴）。
            self.state["hand"] = hand = None
        # 叠加剥削（--exploit）：每手结束把完整观测发给 advisor Profiler（在清空 hand、且任何 HU/
        # 码深漂移早退之前——保证每手都喂，对手统计无遗漏）。--exploit 关 → 不发 → IPC byte-equal。
        if self.exploit and hand is not None:
            try:
                self.advisor.observe(hand.observe_record())
            except Exception as e:
                print(f"  [observe 异常] {e}（跳过本手画像，不影响决策）", file=sys.stderr)
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
        if self.debug:
            my_seat = hand.my_seat if hand is not None else None
            my_final = (final.get(str(my_seat), final.get(my_seat))
                        if my_seat is not None else None)
            self._dbg(f"hand_result winners={msg.get('winners')} pot={msg.get('pot')} "
                      f"final_stacks={final}"
                      + (f" my_stack={my_final}" if my_final is not None else "")
                      + (f" shown={msg.get('shown_cards')}"
                         if msg.get("shown_cards") else ""))
        # 两人桌（HU）检测：final_stacks 键集 = 本手发牌座（盲注跳空座，exec §3.2）→ 恰 2 座
        # = 两人桌。本实现 postflop 不支持 HU → 主动离场、关连接，由 run_real 等 10 分钟后重连
        # （连续最多 5 次）。在码深漂移 rejoin 之前判：HU 要的是断开等待、不是就地换座（同桌还是两人）。
        if isinstance(final, dict) and len(final) == 2:
            print(f"  [两人桌] 本手仅 2 座发牌（final_stacks 键={sorted(final)}）"
                  f"→ 离场，等待后重试", file=sys.stderr)
            self.hu_detected = True
            self.seat_state = "leaving"
            self.send(ws, {"type": "leave_table"})
            self.state["hand"] = None
            ws.close()
            return
        # ≥3 座发牌 = 非两人桌 → 标记本次连接落到了非 HU 桌（run_real 据此把两人桌**连续**
        # 重试计数清零：重连成功换到人多的桌就不再累积，只有连续 5 次仍是两人桌才放弃）。
        if isinstance(final, dict) and len(final) >= 3:
            self.non_hu_hand_seen = True
        # §4 码深漂移：我方栈漂出 [80,180]BB → leave_table + rejoin 取 2000。
        my_stack = None
        if hand is not None:
            my_stack = final.get(str(hand.my_seat), final.get(hand.my_seat))
        if my_stack is not None and (my_stack < STACK_LEAVE_LO or my_stack > STACK_LEAVE_HI):
            self.state["hand"] = None
            self.rejoin(ws, f"[码深漂移] 我方栈 {my_stack} 漂出 [{STACK_LEAVE_LO},{STACK_LEAVE_HI}]")
            return
        self.state["hand"] = None


# ===========================================================================
# 真实联机（WS）
# ===========================================================================
RECONNECT_MAX = 10


def _next_reconnect_attempts(prev, played_hand):
    """断线后的连续重连计数：本次连接打完过一手（hand_result）= 已恢复 → 从 1 重新计；否则累加。"""
    return 1 if played_hand else prev + 1


def _make_ws_send(ws_log=None):
    """真实发送函数：序列化一次，落 ws_log（若开）后原样发出——日志里的 out 与线上逐字节相同。"""
    def send(ws, obj):
        raw = json.dumps(obj)
        if ws_log:
            ws_log.msg("out", raw)
        ws.send(raw)
    return send


def _on_raw_message(session, ws, raw, ws_log=None):
    # 先落盘再解析：服务端发来坏 JSON 时日志里也留得住原文。
    if ws_log:
        ws_log.msg("in", raw)
    session.handle_message(ws, json.loads(raw))


def run_real(advisor, api_key, num_hands, action_log=None, hh_log=None, prewarm=False,
             exploit=False, debug=False, ws_log_path=None):
    import websocket  # 延迟 import：离线 selftest 不需要 websocket-client

    # action / HH / WS 日志均用 append：贯穿全程的后台采集（§4.2），跨重连 / 多次运行累积同一个文件。
    log_f = open(action_log, "a") if action_log else None
    hh_f = open(hh_log, "a") if hh_log else None
    ws_log = WsLog(ws_log_path) if ws_log_path else None
    session = Session(advisor, send=_make_ws_send(ws_log), num_hands=num_hands, log_f=log_f,
                      hh_f=hh_f, prewarm=prewarm, exploit=exploit, debug=debug)

    # 看门狗线程：被移出桌 / 桌散后服务端不再推任何消息，回调模型里没有别的唤醒点。
    stop_watchdog = threading.Event()

    def watchdog_loop():
        n = 0
        while not stop_watchdog.wait(1):
            session.tick()
            n += 1
            if n % 30 == 0:
                session.watchdog_check()

    threading.Thread(target=watchdog_loop, daemon=True).start()

    def on_open(ws):
        if ws_log:
            ws_log.event("open")

    def on_message(ws, raw):
        _on_raw_message(session, ws, raw, ws_log)

    def on_error(ws, err):
        print(f"  [ws error] {err!r}", file=sys.stderr)
        if isinstance(err, KeyboardInterrupt):
            # run_forever 吞掉 Ctrl-C 后正常返回 → 不标记就会被当成断线自动重连（run5 实测）。
            session.stopping = True
        if ws_log:
            ws_log.event("error", error=repr(err))

    def on_close(ws, code, reason):
        print(f"  [ws closed] code={code} reason={reason}", file=sys.stderr)
        if ws_log:
            ws_log.event("close", code=code, reason=reason if isinstance(reason, str) else repr(reason))

    header = [f"Authorization: Bearer {api_key}"]
    # 连接循环：分两类「重连」，计数独立——
    #   · 断线（run_forever 异常返回）：3s 后重连，**连续** RECONNECT_MAX 次才放弃（服务器保留座位
    #     120s，重连后由 Session 走 already_seated → resync 续打）。本次连接打完过一手即清零（_next_reconnect_attempts）——live 实测
    #     2026-10-16 服务端 50 分钟内 RST 10 次、每次重连都正常打牌，累计计数会把整局白白停掉。
    #   · 两人桌（session.hu_detected）：本实现 postflop 不支持 HU → 主动离场，等 HU_RETRY_WAIT_S
    #     （2 分钟）后重连，期望换到人更多的桌；**连续** HU_MAX_RETRIES 次仍是两人桌才放弃。
    #     hu_retries 计的是「连续」次数：本次连接只要打到过非两人桌手（non_hu_hand_seen）就清零
    #     ——重连成功换到人多的桌即视为恢复，不累积。
    reconnect_attempts = 0
    hu_retries = 0
    while session.counters["hands"] < num_hands:
        hands_before = session.counters["hands"]
        session.hu_detected = False
        session.non_hu_hand_seen = False
        if ws_log:
            ws_log.conn += 1
        ws = websocket.WebSocketApp(
            WS_URL, header=header, on_open=on_open,
            on_message=on_message, on_error=on_error, on_close=on_close,
        )
        ws.run_forever(ping_interval=30, ping_timeout=10)
        # 断开后清掉 ws 引用：等待 / 重连期间看门狗读到 None 即 no-op（不往死 ws 发 rejoin）。
        session._ws = None
        if session.counters["hands"] >= num_hands:
            break
        if session.stopping:
            print("  收到中断，不再重连（座位由服务器保留 120s 后释放）。", file=sys.stderr)
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
        reconnect_attempts = _next_reconnect_attempts(
            reconnect_attempts, session.counters["hands"] > hands_before)
        if reconnect_attempts >= RECONNECT_MAX:
            print(f"  断线重连达上限（{reconnect_attempts}），放弃。", file=sys.stderr)
            break
        print(f"  断线，3s 后重连（attempt {reconnect_attempts}）…", file=sys.stderr)
        time.sleep(3)

    stop_watchdog.set()
    if log_f:
        log_f.close()
    if hh_f:
        hh_f.close()
    if ws_log:
        ws_log.close()
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

    # 场景 5b（--ws-log 隔离）：原始字符串走真实收发路径，挂/不挂 WS 日志逐字节一致；日志原文吻合。
    _selftest_ws_log_byte_equal()

    # 场景 5c（断线计数）：连接内打完过一手即清零，只有连续断线才逼近上限。
    _selftest_reconnect_counter()

    # 场景 5d（重连 / 换座状态机）：resync 补事件 + 快照 token 出牌、漂移等 player_left、关桌等重排。
    _selftest_reconnect_and_reseat()

    # 场景 6（短桌幻影座映射）：table_state.in_hand 占座推断 → dealt_seats / placeholder 栈 /
    # 盲注座重算 / HH dealt_est。
    _selftest_short_handed()

    # 场景 6b（不带 amount 的 all_in）：投入按 stack_before 补、剩余栈记 0 → hand-start 真栈正确。
    _selftest_allin_no_amount()

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

    # 场景 9（叠加剥削 observe IPC）：每手结束发 observe → advisor ack（--exploit 关也 ack、不崩）；
    # observe_record 逐动作带街；names 进 request；observe 与 decide 锁步对齐（drain）。剥削画像/收敛/
    # tilt 数学由 Rust 单测覆盖（opponent_profile / subgame），本场景只钉 driver↔advisor IPC 管道。
    hand9 = HandState("h9", button_seat=0, my_seat=1)
    hand9.hole = ["As", "Ks"]
    hand9.on_player_action(2, "raise", 60)        # preflop 对手开池
    hand9.on_player_action(3, "fold", None)
    hand9.on_community(["7h", "2c", "Kd"], "flop")
    hand9.on_player_action(2, "bet", 80)          # flop 对手续注
    hand9.update_name(1, "me")
    hand9.update_name(2, "villain")
    rec9 = hand9.observe_record()
    if rec9["observe"] != 1 or rec9["names"].get(2) != "villain":
        raise RuntimeError(f"[exploit] observe_record 结构异常: {rec9}")
    if not all({"seat", "street", "action"} <= set(a) for a in rec9["actions"]):
        raise RuntimeError(f"[exploit] observe actions 缺字段: {rec9['actions']}")
    if rec9["actions"][0]["street"] != "preflop" or rec9["actions"][-1]["street"] != "flop":
        raise RuntimeError(f"[exploit] 逐动作街标注错: {rec9['actions']}")
    req9 = hand9.build_request(valid_flop, include_names=True)
    if str(req9.get("names", {}).get(2, req9.get("names", {}).get("2"))) != "villain":
        raise RuntimeError(f"[exploit] include_names 应把 names 带进 request: {req9.get('names')}")
    for _ in range(3):
        advisor.observe(rec9)
    if advisor._pending_prewarms != 3:
        raise RuntimeError(f"[exploit] observe×3 后应有 3 条待 drain，得 {advisor._pending_prewarms}")
    resp9 = advisor.decide(req7)  # 复用场景 7 的合法 flop 请求；先 drain 3 条 observe ack
    if advisor._pending_prewarms != 0:
        raise RuntimeError(f"[exploit] decide 后 observe ack 应 drain 净，得 {advisor._pending_prewarms}")
    _assert_legal(resp9, valid_flop, "exploit observe+decide")
    print(f"[selftest 9 exploit] observe×3 acked+drained, names 进 request, resp={resp9} "
          f"(--exploit 开则 advisor 进程内累积画像；剥削数学由 Rust 单测覆盖)", file=sys.stderr)

    print("OK: 9 个 canned 场景跑通：advisor 全程出合法动作、driver 组请求/动作包（含真栈 stacks"
          " + 短桌 dealt_seats）正常、HH 日志 byte-equal 隔离成立、prewarm IPC 锁步对齐、"
          "两人桌检测触发离场、叠加剥削 observe IPC 锁步对齐。", file=sys.stderr)


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


def _selftest_ws_log_byte_equal():
    """场景 5b（--ws-log 隔离）：canned 整手序列以原始字符串走真实收发路径（_on_raw_message +
    _make_ws_send），挂 / 不挂 --ws-log 各跑一遍——advisor 请求/响应流 + 线上发出的字节逐字节一致；
    日志 in 行 = 收到的原文（逐条、同序），out 行 = 线上发出的原文，event 行原样落。"""
    import os
    import tempfile

    class _RecWs(_FakeWs):
        def __init__(self):
            self.sent = []

        def send(self, raw):
            self.sent.append(raw)

    # 收到的原文故意用非默认格式（无空格 + 中文不转义），验证落盘的是原文而不是重新编码。
    raws = [json.dumps(m, ensure_ascii=False, separators=(",", ":")) for m in _canned_hh_messages()]

    def run_once(ws_log):
        stub = _StubAdvisor()
        session = Session(stub, send=_make_ws_send(ws_log), num_hands=10 ** 9,
                          log_f=None, hh_f=None)
        ws = _RecWs()
        for raw in raws:
            _on_raw_message(session, ws, raw, ws_log)
        return stub.transcript, ws.sent

    t_off, sent_off = run_once(None)
    with tempfile.TemporaryDirectory() as d:
        path = os.path.join(d, "ws.jsonl")
        ws_log = WsLog(path)
        ws_log.conn = 1
        ws_log.event("open")
        t_on, sent_on = run_once(ws_log)
        ws_log.event("close", code=1000, reason="bye")
        ws_log.close()
        with open(path) as f:
            lines = [json.loads(line) for line in f]

    if t_off != t_on:
        raise RuntimeError("[ws-log] 挂/不挂 WS 日志 advisor 请求/响应流不 byte-equal")
    if sent_off != sent_on:
        raise RuntimeError("[ws-log] 挂/不挂 WS 日志 线上发出的字节不一致")
    if not sent_on:
        raise RuntimeError("[ws-log] canned 序列应至少发出 1 个 action 包")
    ins = [r["raw"] for r in lines if r["dir"] == "in"]
    outs = [r["raw"] for r in lines if r["dir"] == "out"]
    events = [r["event"] for r in lines if r["dir"] == "event"]
    if ins != raws:
        raise RuntimeError(f"[ws-log] in 行应逐条等于收到的原文（{len(ins)} vs {len(raws)}）")
    if outs != sent_on:
        raise RuntimeError(f"[ws-log] out 行应逐条等于线上发出的原文（{len(outs)} vs {len(sent_on)}）")
    if events != ["open", "close"] or lines[-1].get("code") != 1000:
        raise RuntimeError(f"[ws-log] event 行错: {events} / {lines[-1]}")
    if any(r.get("conn") != 1 or not isinstance(r.get("ts"), float) for r in lines):
        raise RuntimeError("[ws-log] 每行须带 conn=1 + 浮点 ts")
    print(f"[selftest 5b WS-log byte-equal] 挂/不挂 --ws-log：advisor 请求/响应 + 线上字节逐字节一致；"
          f"日志 {len(ins)} in / {len(outs)} out / {len(events)} event，原文逐条吻合。", file=sys.stderr)


def _selftest_reconnect_counter():
    n = 0
    for played in (True, True, False, True, False, False):
        n = _next_reconnect_attempts(n, played)
    if n != 3:
        raise RuntimeError(f"[reconnect] 打过手后应从 1 重新计，连续 2 次空断后应为 3，得 {n}")
    n = 0
    for _ in range(RECONNECT_MAX):
        n = _next_reconnect_attempts(n, False)
    if n != RECONNECT_MAX:
        raise RuntimeError(f"[reconnect] 连续 {RECONNECT_MAX} 次空断应达上限，得 {n}")
    # live 2026-10-16 序列：10 次断线、其中 9 次连接打过手 → 计数始终 ≤ 2，不放弃。
    n, peak = 0, 0
    for played in (True, True, True, True, True, True, True, False, True, True):
        n = _next_reconnect_attempts(n, played)
        peak = max(peak, n)
    if peak >= RECONNECT_MAX:
        raise RuntimeError(f"[reconnect] live 序列不应触发放弃，峰值 {peak}")
    print(f"[selftest 5c 断线计数] 打过手即清零；连续 {RECONNECT_MAX} 次空断才放弃；"
          f"live 10 次断线序列峰值 {peak}。", file=sys.stderr)


def _selftest_reconnect_and_reseat():
    """场景 5d（重连 / 换座状态机，run5 2026-10-17 实测问题）：每条路径只发必要的消息，
    不在 already_seated / leave_pending 上回发 leave+join；断线仍在座 → resync 补事件 +
    按快照 token 出牌一次；漂移换座等自己的 player_left 再进大厅；关桌等服务器重排。"""
    import io

    T = "tbl-rc"

    def mk(hh_f=None):
        stub = _StubAdvisor()
        sent = []
        s = Session(stub, send=lambda ws, obj: sent.append(obj), num_hands=10 ** 9,
                    log_f=None, hh_f=hh_f)
        return s, stub, sent, _FakeWs()

    def pa(seat, action, amount, seq, stack=2000, name=None):
        m = {"type": "player_action", "seat": seat, "action": action, "amount": amount,
             "street": "preflop", "stack": stack, "table_id": T, "table_seq": seq}
        if name:
            m["name"] = name
        return m

    def types(sent):
        return [o.get("type") for o in sent]

    # ① 断线后仍在座：connected → join → already_seated → 一次 resync；先到的 your_turn 暂存；
    #   resync 补发断线期间的动作（含重复）后按快照 token 出牌一次，暂存的同 token 不再出。
    s, stub, sent, ws = mk()
    s.handle_message(ws, {"type": "connected", "name": "me"})
    s.handle_message(ws, {"type": "table_joined", "table_id": T, "seat": 2, "table_seq": 1})
    s.handle_message(ws, {"type": "hand_start", "hand_id": "h1", "seat": 2, "dealer_seat": 0,
                          "table_id": T, "table_seq": 10})
    s.handle_message(ws, {"type": "hole_cards", "cards": ["Ah", "Kd"], "table_id": T,
                          "table_seq": 11})
    s.handle_message(ws, pa(3, "fold", None, 12))
    # —— 断线 ——（13–16 没收到）；重连后服务器先推快照，再 connected
    s.handle_message(ws, {"type": "table_state", "table_id": T, "table_seq": 20, "hand_id": "h1",
                          "street": "preflop", "hero": {"seat": 2}})
    s.handle_message(ws, {"type": "connected", "name": "me"})
    seated_err = {"type": "error", "code": "already_seated", "message": "You are already at a table",
                  "table_id": T, "seat": 2}
    s.handle_message(ws, seated_err)
    s.handle_message(ws, dict(seated_err))  # 重复的 already_seated 不得再发任何东西
    valid = [{"action": "fold"}, {"action": "call", "amount": 20},
             {"action": "raise", "min": 40, "max": 2000}]
    s.handle_message(ws, {"type": "your_turn", "hand_id": "h1", "turn_token": "tt-9", "seat": 2,
                          "valid_actions": valid, "min_raise": 40, "max_raise": 2000})
    if types(sent) != ["join_lobby", "join_lobby", "resync_request"]:
        raise RuntimeError(f"[reconnect] 仍在座应只发 join + join + resync，得 {types(sent)}")
    # last_table_seq = 断线前最后一条**事件**（12），不是重连时快照的 20。
    if sent[2] != {"type": "resync_request", "table_id": T, "last_table_seq": 12}:
        raise RuntimeError(f"[reconnect] resync_request 字段错: {sent[2]}")
    s.handle_message(ws, {
        "type": "resync_response", "role": "player", "from_table_seq": 12, "to_table_seq": 30,
        "replayed_events": [pa(5, "fold", None, 14), pa(4, "fold", None, 13), pa(3, "fold", None, 12),
                            pa(0, "fold", None, 15), pa(1, "call", 20, 16, stack=1980),
                            {"type": "your_turn", "turn_token": "old", "table_seq": 17}],
        "snapshot": {"type": "table_state", "table_id": T, "table_seq": 30, "hand_id": "h1",
                     "street": "preflop", "board": [],
                     "seats": [{"seat": i, "name": f"p{i}", "stack": 1980 if i in (1, 2) else 2000,
                                "in_hand": i in (1, 2)} for i in range(6)],
                     "hero": {"seat": 2, "hole_cards": ["Ah", "Kd"], "turn_token": "tt-9",
                              "valid_actions": valid}}})
    acts = [o for o in sent if o.get("type") == "action"]
    if len(acts) != 1 or acts[0]["turn_token"] != "tt-9" or acts[0]["hand_id"] != "h1":
        raise RuntimeError(f"[reconnect] 应按快照 token tt-9 出牌恰好一次，得 {acts}")
    seats = [a["seat"] for a in s.state["hand"].actions]
    if seats[:5] != [3, 4, 5, 0, 1]:
        raise RuntimeError(f"[reconnect] 补发后动作序应为 3,4,5,0,1（去重 + 排序），得 {seats}")
    if len(stub.transcript) != 1 or '"seat": 1, "action": "call"' not in stub.transcript[0][0]:
        raise RuntimeError("[reconnect] advisor 应在补全历史后被问一次")
    s.handle_message(ws, pa(0, "fold", None, 15))  # 实时流里迟到的重复事件
    if s.counters["dup_events"] < 2 or len(s.state["hand"].actions) != len(seats):
        raise RuntimeError(f"[reconnect] table_seq 去重失效 dup={s.counters['dup_events']}")
    if any(o.get("type") == "leave_table" for o in sent):
        raise RuntimeError("[reconnect] 仍在座时不得发 leave_table")

    # ② resync 超时：暂存的 your_turn 由 tick 按现有历史出牌；③ resync 被拒（not_at_table）→ 进大厅、不出牌。
    s, stub, sent, ws = mk()
    s.handle_message(ws, {"type": "connected", "name": "me"})
    s.handle_message(ws, {"type": "hand_start", "hand_id": "h2", "seat": 2, "dealer_seat": 0,
                          "table_id": T, "table_seq": 5})
    s.handle_message(ws, {"type": "hole_cards", "cards": ["7h", "2c"], "table_id": T, "table_seq": 6})
    s.handle_message(ws, seated_err)
    s.handle_message(ws, {"type": "your_turn", "hand_id": "h2", "turn_token": "tt-2", "seat": 2,
                          "valid_actions": valid, "min_raise": 40, "max_raise": 2000})
    s.resync_pending_ts -= RESYNC_WAIT_S + 1
    s.tick()
    if [o.get("turn_token") for o in sent if o.get("type") == "action"] != ["tt-2"]:
        raise RuntimeError(f"[reconnect] resync 超时后应对暂存的 your_turn 出牌，得 {sent}")
    s, stub, sent, ws = mk()
    s.handle_message(ws, {"type": "connected", "name": "me"})
    s.handle_message(ws, seated_err)
    s.handle_message(ws, {"type": "your_turn", "hand_id": "h3", "turn_token": "tt-3",
                          "valid_actions": valid})
    s.handle_message(ws, {"type": "error", "code": "not_at_table", "message": "You are not at a table"})
    if types(sent) != ["join_lobby", "resync_request", "join_lobby"]:
        raise RuntimeError(f"[reconnect] resync 被拒应只重新 join、不出牌，得 {types(sent)}")

    # ④ 码深漂移：只发 leave；leave 生效前的 already_seated / 新手开局不触发任何发送；
    #   自己的 player_left 后 join 一次；随后对 leave 的 not_at_table 回执不再 join。
    s, stub, sent, ws = mk()
    s.handle_message(ws, {"type": "connected", "name": "me"})
    s.handle_message(ws, {"type": "table_joined", "table_id": T, "seat": 2, "table_seq": 1})
    s.handle_message(ws, {"type": "hand_start", "hand_id": "h4", "seat": 2, "dealer_seat": 0,
                          "table_id": T, "table_seq": 2})
    s.handle_message(ws, {"type": "hand_result", "hand_id": "h4", "table_id": T, "table_seq": 3,
                          "final_stacks": {"0": 2000, "1": 1000, "2": 4000}})
    if types(sent) != ["join_lobby", "leave_table"]:
        raise RuntimeError(f"[reseat] 漂移应只发 leave_table，得 {types(sent)}")
    s.handle_message(ws, seated_err)
    s.handle_message(ws, {"type": "error", "code": "leave_pending",
                          "message": "You will leave after the current hand"})
    s.handle_message(ws, {"type": "hand_start", "hand_id": "h5", "seat": 2, "dealer_seat": 1,
                          "table_id": T, "table_seq": 4})
    if types(sent) != ["join_lobby", "leave_table"] or s.seat_state != "leaving":
        raise RuntimeError(f"[reseat] leave 生效前不得再发消息，得 {types(sent)} {s.seat_state}")
    s.handle_message(ws, {"type": "player_left", "seat": 4, "name": "other", "reason": "left",
                          "table_id": T, "table_seq": 5})
    s.handle_message(ws, {"type": "player_left", "seat": 2, "name": "me", "reason": "left",
                          "table_id": T, "table_seq": 6})
    s.handle_message(ws, {"type": "error", "code": "not_at_table", "message": "You are not at a table"})
    s.handle_message(ws, {"type": "lobby_joined", "position": 1})
    s.handle_message(ws, {"type": "table_joined", "table_id": "tbl-2", "seat": 0, "table_seq": 1})
    if types(sent) != ["join_lobby", "leave_table", "join_lobby"] or s.seat_state != "seated":
        raise RuntimeError(f"[reseat] 离桌确认后应 join 一次，得 {types(sent)} {s.seat_state}")

    # ⑤ 关桌：不发 join（服务器自动重排），之后自己的 player_left 也不触发 join；
    #   旧手的 hand_result（hand_id 对不上）不得挂到新手上。
    hh_buf = io.StringIO()
    s, stub, sent, ws = mk(hh_buf)
    s.handle_message(ws, {"type": "connected", "name": "me"})
    s.handle_message(ws, {"type": "hand_start", "hand_id": "h6", "seat": 1, "dealer_seat": 0,
                          "table_id": T, "table_seq": 2})
    s.handle_message(ws, {"type": "table_closed", "reason": "Not enough players",
                          "table_id": T, "table_seq": 3})
    s.handle_message(ws, {"type": "player_left", "seat": 1, "name": "me", "reason": "left",
                          "table_id": T, "table_seq": 4})
    s.handle_message(ws, {"type": "lobby_joined", "position": 1})
    s.handle_message(ws, {"type": "table_joined", "table_id": "tbl-3", "seat": 3, "table_seq": 1})
    if types(sent) != ["join_lobby"] or s.state["hand"] is not None or s.seat_state != "seated":
        raise RuntimeError(f"[reseat] 关桌应等服务器重排、不发 join，得 {types(sent)}")
    s.handle_message(ws, {"type": "hand_start", "hand_id": "h7", "seat": 3, "dealer_seat": 0,
                          "table_id": "tbl-3", "table_seq": 2})
    s.handle_message(ws, {"type": "hand_result", "hand_id": "h6", "table_id": "tbl-3",
                          "table_seq": 3, "final_stacks": {"3": 9999, "1": 1, "2": 1}})
    if s.counters["hh_skipped"] != 1 or hh_buf.getvalue() \
            or any(o.get("type") == "leave_table" for o in sent):
        raise RuntimeError("[reseat] hand_id 对不上的 hand_result 应按跳过处理、不触发漂移换座")

    # ⑥ 看门狗：离桌迟迟没有 player_left → 当作已离开、join 一次；没有手的 your_turn → 安全出牌。
    s, stub, sent, ws = mk()
    s.handle_message(ws, {"type": "connected", "name": "me"})
    s.handle_message(ws, {"type": "table_joined", "table_id": T, "seat": 2, "table_seq": 1})
    s.leave(ws, "selftest")
    s.leave_ts -= LEAVE_TIMEOUT_S + 1
    s.watchdog_check()
    s.watchdog_check()
    if types(sent) != ["join_lobby", "leave_table", "join_lobby"]:
        raise RuntimeError(f"[reseat] 离桌超时应 join 一次，得 {types(sent)}")
    s.handle_message(ws, {"type": "your_turn", "hand_id": "hx", "turn_token": "tt-x",
                          "valid_actions": [{"action": "fold"}, {"action": "call", "amount": 20}]})
    s.handle_message(ws, {"type": "your_turn", "hand_id": "hy", "turn_token": "tt-y",
                          "valid_actions": [{"action": "check"}, {"action": "bet", "min": 20}]})
    safe = [(o["turn_token"], o["action"]) for o in sent if o.get("type") == "action"]
    if safe != [("tt-x", "fold"), ("tt-y", "check")] or stub.transcript:
        raise RuntimeError(f"[reseat] 没有手牌状态时应过牌 / 弃牌且不问 advisor，得 {safe}")
    print("[selftest 5d 重连 / 换座] 仍在座 → 一次 resync、补发去重排序、按快照 token 出牌一次；"
          "resync 超时 / 被拒；漂移等 player_left 再 join；关桌等重排；离桌超时；无手安全出牌。",
          file=sys.stderr)


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


def _selftest_allin_no_amount():
    """场景 6b：服务端 all_in 消息不带 amount，stack / stack_before / stack_after 都是全下前的剩余栈、
    contribution_delta=0（2026-10-16 实测）。修前投入记 0、your_turn 又把该座栈更新成 0 →
    hand-start 栈只剩盲注（run3：BB 被算成 44 → advisor auth_seat_mismatch 放弃）。满桌 6 座，
    button=0：UTG(3) raise 60 → 4/5/0 fold → SB(1) 全下（剩余 1990）→ 轮到我（BB=2）。"""
    msgs = [
        {"type": "hand_start", "hand_id": "ai-1", "seat": 2, "dealer_seat": 0,
         "table_id": "tbl-ai", "blinds": {"small_blind": 10, "big_blind": 20}},
        {"type": "hole_cards", "cards": ["Ah", "Kd"]},
        {"type": "player_action", "seat": 3, "action": "raise", "amount": 60,
         "street": "preflop", "stack": 1940, "stack_before": 2000, "stack_after": 1940,
         "contribution_delta": 60},
        {"type": "player_action", "seat": 4, "action": "fold", "amount": None,
         "street": "preflop", "stack": 2000, "contribution_delta": 0},
        {"type": "player_action", "seat": 5, "action": "fold", "amount": None,
         "street": "preflop", "stack": 2000, "contribution_delta": 0},
        {"type": "player_action", "seat": 0, "action": "fold", "amount": None,
         "street": "preflop", "stack": 2000, "contribution_delta": 0},
        {"type": "player_action", "seat": 1, "action": "all_in", "amount": None,
         "amount_mode": "to_total", "street": "preflop", "stack": 1990, "stack_before": 1990,
         "stack_after": 1990, "contribution_delta": 0, "to_call_before": 50},
        {"type": "your_turn", "hand_id": "ai-1", "turn_token": "tt-ai", "seat": 2,
         "players": [{"seat": 0, "name": "p0", "stack": 2000},
                     {"seat": 1, "name": "p1", "stack": 0},
                     {"seat": 2, "name": "me", "stack": 1980},
                     {"seat": 3, "name": "p3", "stack": 1940},
                     {"seat": 4, "name": "p4", "stack": 2000},
                     {"seat": 5, "name": "p5", "stack": 2000}],
         "valid_actions": [{"action": "fold"}, {"action": "call"}],
         "min_raise": None, "max_raise": None},
    ]
    stub = _StubAdvisor()
    session = Session(stub, send=lambda ws, obj: None, num_hands=10 ** 9, log_f=None, hh_f=None)
    ws = _FakeWs()
    for m in msgs:
        session.handle_message(ws, m)
    req = json.loads(stub.transcript[0][0])
    if req.get("stacks") != [2000] * 6:
        raise RuntimeError(f"[all_in] 不带 amount 的全下后真栈回推错: {req.get('stacks')}")
    if req["actions"][-1] != {"seat": 1, "action": "all_in"}:
        raise RuntimeError(f"[all_in] 历史末尾应是 SB all_in: {req['actions']}")
    print("[selftest 6b all_in 无 amount] 投入按 stack_before 补 → 真栈 6×2000", file=sys.stderr)


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
    p = argparse.ArgumentParser(description="OpenPoker N-max WS driver + Rust advisor")
    add_advisor_args(
        p, default_seats=6,
        debug_help="打印牌局中间数据到 stderr（driver 侧：手起/发牌/对手动作/决策请求+响应/"
                   "实发动作/结果）+ 透传 advisor（内部：座位映射/lockstep 同步/真栈 auth/"
                   "range 来源+per-seat 摘要/solve）；纯展示、不改 IPC（byte-equal 不变）")
    p.add_argument("--num-hands", type=int, default=100)
    p.add_argument("--api-key", default=None, help="OpenPoker Bearer api_key（真实联机用；注册见 docstring）")
    p.add_argument("--action-log", default="openpoker_actions.jsonl",
                   help="每决策落 {req,resp,sent} JSONL；空串关闭")
    p.add_argument("--hh-log", default="openpoker_hh.jsonl",
                   help="每手落全桌 HH JSONL（append 累积；§4.2 数据管道）；空串关闭")
    p.add_argument("--ws-log", default=None,
                   help="与服务端往来的原始 WS 消息逐条落 JSONL（收/发 + 连接开关事件，append 累积）；"
                        "缺省关；纯旁路、不改 IPC / 出动作字节")
    p.add_argument("--selftest", action="store_true", help="离线验 IPC，不连网（无需账号）")
    args = p.parse_args()

    validate_advisor_args(args)
    if args.selftest and args.seats != 6:
        raise SystemExit("--selftest 的 canned 场景按 6 座写，只支持 --seats 6")
    global NUM_SEATS
    NUM_SEATS = args.seats

    exploit_on = exploit_enabled(args)
    # 暖启动剥削画像：--hh-log 指定的文件若已有历史牌局（同一文件 append 跨 run 累积），
    # 启动期喂给 advisor Profiler（advisor 侧逐手重放，网络丢数据 / 不完整的手会被过滤掉、
    # 不污染统计）。仅真实联机（非 selftest）+ 文件存在且非空时透传。
    warm_hh = None
    if (exploit_on and not args.selftest and args.hh_log
            and os.path.exists(args.hh_log) and os.path.getsize(args.hh_log) > 0):
        warm_hh = args.hh_log
    advisor = start_advisor(args, advisor_extra_args(args, warm_profile_hh=warm_hh))
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
                     prewarm=args.search_prewarm, exploit=exploit_on,
                     debug=args.debug_log, ws_log_path=args.ws_log or None)
    finally:
        advisor.close()


if __name__ == "__main__":
    main()

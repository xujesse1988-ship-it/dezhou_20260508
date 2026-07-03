#!/usr/bin/env python3
"""启发式 9-max 决策日志分析（迭代闭环的「看数据」环节）。

吃 driver 的 action-log（每行 `{req, resp, sent}`，与 6-max openpoker_actions 同格式），
聚合决策质量指标 + 自动标漏洞，指导调 config / 加规则。

指标：
  - 决策量、按街分布
  - 动作频率（总 + 按街）
  - source 分布（top）+ fallback 率/原因
  - 翻前 VPIP / PFR（hero 自愿入池）+ 按位置桶 open/3bet 率
  - 激进度 AF =（bet+raise）/ call（总 + 翻后）
  - equity 分桶（**按街拆**：翻前=vs-random 标度，翻后=vs-range 标度，不可跨街比）
  - 面对下注的弃/跟 EV 余量 surplus=equity-required_eq（required_eq 来自重建底池实际价位）
  - 自动漏洞标记（fallback 偏高、过松/过紧按实际价位判、激进度异常…）
  - Phase A 节点聚合（docs/scenario_heuristic_design.md §6）：resp.trace 的节点路径 →
    每节点 (机会数 n, EV 均值, CI95, 动作分布)。EV 归因口径=整手 net 摊给该手命中的所有
    节点（§8②，用 hand_result 行的 net 按 sent.hand_id 关联）；另报兜底率（落 fallback
    或 legalize 降级的决策占比，§8④）。旧日志（无 trace 字段）优雅降级：跳过本节不崩。

pot/to_call 来源：优先读 resp 里引擎记的 pot/to_call/required_eq/opps_in_pot（精确）；旧日志
（无这些字段）则用 reconstruct_pot_state 从 req 的 actions[] 重放（与 Rust from_request 等价）。

用法：
  python3 tools/heuristic9_analyze.py heuristic9_actions.jsonl
  python3 tools/heuristic9_analyze.py openpoker9_actions_*.jsonl   # 也吃旧 6-max 格式
"""
import json
import math
import sys
from collections import Counter, defaultdict

# 节点 EV 读数的样本纪律（设计 §4.3）：EV 归因手数低于此门槛的节点标「待观察」，禁止下结论。
NODE_MIN_HANDS = 30

_RANK_ORD = {r: i for i, r in enumerate("23456789TJQKA")}
PREMIUM = {"AA", "KK", "QQ", "JJ", "AK"}

# 软 flag 判定容差：surplus 必须越过 ±margin 才报，吸收 equity 蒙特卡洛抖动 + 重建近似。
EV_MARGIN = 0.08
# 翻后接近 0 equity 还跟注的硬伤阈值（与 hard_issues 的 multiplicity 处理对齐）。
NEAR_ZERO_EQ = 0.12

# 默认栈 = 100bb（与 Rust view::default_stack_chips 对齐：栈未知/为 0 时用）。
DEFAULT_STACK_BB = 100


def reconstruct_pot_state(req):
    """从 req 重放下注状态 → (pot, to_call, required_eq, opps_in_pot)。

    与 Rust `view::GameView::from_request` 的 pot/to_call/pot_odds 等价（同一份 actions[] 历史）：
    逐街分段重放、每座 committed、检测下注轮关闭后推进街、以 board 长度校准目标街。
    供缺 pot/to_call 字段的旧日志（run5–9）重建用；新日志直接读 resp 字段更精确。
    返回 None 表示信息不足（无 big_blind 等）。
    """
    try:
        n = max(int(req.get("num_seats", 0)), 2)
        bb = max(int(req.get("big_blind", 0)), 1)
        sb = max(int(req.get("small_blind", 0)), 1)
    except (TypeError, ValueError):
        return None
    if not req.get("big_blind"):
        return None
    my = int(req.get("my_seat", 0)) % n
    btn = int(req.get("button_seat", 0)) % n

    default_stack = max(bb * DEFAULT_STACK_BB, 1)
    start_stack = [default_stack] * n
    stacks = req.get("stacks", []) or []
    for i, s in enumerate(stacks):
        if i < n and isinstance(s, (int, float)) and s > 0:
            start_stack[i] = int(s)

    if n == 2:
        sb_seat, bb_seat = btn, (btn + 1) % n
    else:
        sb_seat, bb_seat = (btn + 1) % n, (btn + 2) % n

    folded = [False] * n
    allin = [False] * n
    committed_total = [0] * n
    committed_round = [0] * n
    has_acted = [False] * n

    def post(seat, amt):
        avail = start_stack[seat]
        pay = min(amt, avail)
        committed_total[seat] += pay
        committed_round[seat] += pay
        if pay >= avail:
            allin[seat] = True

    post(sb_seat, sb)
    post(bb_seat, bb)
    high_bet = max(committed_round) if committed_round else bb
    cur_aggressor = None
    street_idx = 0
    pending_advance = False

    def round_closed():
        live = 0
        for s in range(n):
            if folded[s] or allin[s] or start_stack[s] == 0:
                continue
            live += 1
            if not has_acted[s] or committed_round[s] != high_bet:
                return False
        return live >= 1

    def advance_street():
        nonlocal high_bet, cur_aggressor, street_idx
        for s in range(n):
            committed_round[s] = 0
            has_acted[s] = False
        high_bet = 0
        cur_aggressor = None
        street_idx += 1

    for a in req.get("actions", []) or []:
        if pending_advance:
            advance_street()
            pending_advance = False
        s = int(a.get("seat", 0)) % n
        act = str(a.get("action", "")).lower()
        if act == "fold":
            folded[s] = True
            has_acted[s] = True
        elif act == "check":
            has_acted[s] = True
        elif act == "call":
            need = max(high_bet - committed_round[s], 0)
            avail = max(start_stack[s] - committed_total[s], 0)
            pay = min(need, avail)
            committed_round[s] += pay
            committed_total[s] += pay
            if pay >= avail:
                allin[s] = True
            has_acted[s] = True
        elif act in ("bet", "raise", "allin", "all_in", "all-in"):
            to = a.get("to")
            if to is None:
                if act.startswith("all"):
                    to = committed_total[s] + max(start_stack[s] - committed_total[s], 0)
                else:
                    to = max(high_bet, bb)
            to = min(int(to), start_stack[s])
            delta = max(to - committed_round[s], 0)
            room = max(start_stack[s] - committed_total[s], 0)
            pay = min(delta, room)
            committed_round[s] += pay
            committed_total[s] += pay
            if committed_total[s] >= start_stack[s]:
                allin[s] = True
            if committed_round[s] > high_bet:
                high_bet = committed_round[s]
                cur_aggressor = s
                for t in range(n):
                    if t != s and not folded[t] and not allin[t]:
                        has_acted[t] = False
            has_acted[s] = True
        else:
            has_acted[s] = True
        if round_closed():
            pending_advance = True
    if pending_advance:
        advance_street()

    # 以 board 长度校准目标街（与 Rust 一致，重放漂移时信 board）。
    board = req.get("board", []) or []
    target = {0: 0, 3: 1, 4: 2, 5: 3}.get(len(board), street_idx)
    while street_idx < target:
        advance_street()

    pot = sum(committed_total)
    to_call = max(high_bet - committed_round[my], 0)
    required_eq = to_call / (pot + to_call) if to_call > 0 else 0.0
    opps_in_pot = sum(1 for s in range(n) if s != my and not folded[s])
    return {"pot": pot, "to_call": to_call,
            "required_eq": required_eq, "opps_in_pot": opps_in_pot}


def pot_metrics(req, resp):
    """取决策点的 pot/to_call/required_eq/opps_in_pot：优先读引擎已记的字段（精确），
    否则从 req 重建（向后兼容旧日志）。返回 dict 或 None（信息不足）。"""
    if isinstance(resp.get("required_eq"), (int, float)) and resp.get("to_call") is not None:
        return {
            "pot": resp.get("pot"),
            "to_call": resp.get("to_call"),
            "required_eq": float(resp["required_eq"]),
            "opps_in_pot": resp.get("opps_in_pot"),
            "src": "logged",
        }
    rec = reconstruct_pot_state(req)
    if rec is None:
        return None
    rec["src"] = "rebuilt"
    return rec


def hole_label(hole):
    """["As","Kh"] -> "AK"（按 rank 降序，忽略花色）。用于 premium 判定。"""
    if not hole or len(hole) < 2:
        return None
    rs = sorted((c[0] for c in hole if c), key=lambda r: _RANK_ORD.get(r, -1), reverse=True)
    return "".join(rs[:2]) if len(rs) >= 2 else None


def hard_issues(req, resp):
    """逐决策硬伤检测：只报「与样本量无关、几乎必错」的红旗（前期抓 bug 用）。"""
    out = []
    act = resp.get("action")
    src = str(resp.get("source", ""))
    eq = resp.get("equity")
    valid = req.get("valid", {}) or {}
    board = req.get("board", []) or []
    street = resp.get("street") or _street_from_board(board)
    hole = req.get("hole", []) or []
    eqf = eq if isinstance(eq, (int, float)) else None

    # 1) 引擎兜底 = 决策管线某处异常。
    if src.startswith("fallback"):
        out.append(("FALLBACK", src[:50]))
    # 2) 能免费 check 却 fold —— 永远是硬伤（弃牌一无所得）。
    if act == "fold" and valid.get("can_check"):
        out.append(("FOLD_WHILE_CAN_CHECK", f"{street} 可 check 却 fold"))
    # 3) 翻前弃顶级起手（AA/KK/QQ/JJ/AK）—— 几乎必错。
    if street == "preflop" and act == "fold":
        lbl = hole_label(hole)
        if lbl in PREMIUM:
            out.append(("PREMIUM_PREFLOP_FOLD", f"弃 {''.join(hole)}"))
    # 4) 自称 value 却低 equity —— 自相矛盾（翻后 equity 为 vs-range）。
    if "value" in src and eqf is not None and eqf < 0.45:
        out.append(("VALUE_LOW_EQ", f"{src} eq={eqf:.2f}"))
    # 5) 加注金额越界（advisor 本应夹取，越界=映射 bug）。
    if act == "raise":
        amt = resp.get("amount")
        mn, mx = valid.get("min_raise"), valid.get("max_raise")
        if amt is None:
            out.append(("RAISE_NO_AMOUNT", src[:40]))
        elif mn is not None and amt < mn - 0.5:
            out.append(("RAISE_BELOW_MIN", f"to {amt} < min {mn}"))
        elif mx is not None and amt > mx + 0.5:
            out.append(("RAISE_ABOVE_MAX", f"to {amt} > max {mx}"))
    # 6) 近乎 0 equity 还跟注 —— 只在近 HU 池报。多人池 equity 天然被稀释（5 人池均分=20%），
    #    且常是廉价收尾跟注（BB 防守 9:1 价位），raw equity 低不等于 -EV → 多人不报，避免误报。
    #    multiplicity 与软 flag 共用 pot_metrics 的 opps_in_pot（同一份重建口径，两套逻辑一致）。
    if act == "call" and eqf is not None and eqf < NEAR_ZERO_EQ:
        m = pot_metrics(req, resp)
        opps = m.get("opps_in_pot") if m else None
        if opps is not None and opps <= 1:
            out.append(("CALL_NEAR_ZERO_EQ", f"{street} HU eq={eqf:.2f}"))
    return out


def hard_report(paths):
    """硬伤扫描（--hard）：简洁、可频繁跑（每 ~100 手一次）。"""
    cats = Counter()
    examples = []
    total = 0
    for path in paths:
        try:
            f = open(path)
        except OSError as e:
            print(f"跳过 {path}: {e}", file=sys.stderr)
            continue
        for ln, line in enumerate(f, 1):
            line = line.strip()
            if not line:
                continue
            try:
                o = json.loads(line)
            except Exception:
                continue
            req, resp = o.get("req"), o.get("resp")
            if not req or not resp:
                continue
            total += 1
            for cat, detail in hard_issues(req, resp):
                cats[cat] += 1
                if len(examples) < 25:
                    hole = "".join(req.get("hole", []) or [])
                    board = "".join(req.get("board", []) or [])
                    examples.append(f"  [{cat}] {hole}|{board or '-'} "
                                    f"-> {resp.get('action')} {resp.get('amount') or ''} :: {detail}")
        f.close()

    print("=" * 60)
    print(f"硬伤扫描 | 决策 {total} | 硬伤 {sum(cats.values())}")
    print("=" * 60)
    if not cats:
        print("✅ 无硬伤（fallback / 该 check 却弃 / 弃顶对子 / value 低eq / 加注越界 / 跟空气 全 0）")
        return 0
    print("[按类别]")
    for cat, n in cats.most_common():
        print(f"  {cat:24s} {n}")
    print("[样例]")
    for e in examples:
        print(e)
    print(f"\n⚠ 发现 {sum(cats.values())} 处硬伤 —— 逐类排查（多为决策逻辑或映射 bug）。")
    return sum(cats.values())


def pos_bucket(button, my_seat, n):
    """与 Rust view::my_pos_bucket 对齐的位置桶。"""
    idx = (my_seat - button) % n
    if n == 2:
        return "SB" if idx == 0 else "BB"
    if idx == 1:
        return "SB"
    if idx == 2:
        return "BB"
    if idx == 0 or idx == n - 1:
        return "LATE"
    if idx >= n - 3:
        return "MID"
    return "EARLY"


def preflop_num_raises(actions):
    n = 0
    high = 1  # bb 量级（相对）
    # 简单：数 raise 段（to 递增的 raise）。
    last_to = 0
    for a in actions:
        if a.get("action") in ("raise", "bet", "all_in", "allin"):
            to = a.get("to")
            if to is not None and to > last_to:
                n += 1
                last_to = to
    return n


def main():
    argv = sys.argv[1:]
    hard_only = "--hard" in argv
    paths = [a for a in argv if a != "--hard"]
    if not paths:
        print(__doc__)
        sys.exit(1)
    if hard_only:
        sys.exit(1 if hard_report(paths) else 0)

    total = 0
    by_street = Counter()
    action_by_street = defaultdict(Counter)
    sources = Counter()
    fallback = 0
    fallback_reasons = Counter()
    af_bets = 0
    af_calls = 0
    af_bets_post = 0
    af_calls_post = 0
    # 翻前 hero 自愿入池/加注。
    pre_decisions = 0       # hero 翻前首次决策的手数（近似：每个 preflop 决策都算入池机会）
    pre_vpip = 0
    pre_pfr = 0
    open_by_pos = defaultdict(lambda: [0, 0])   # pos -> [open_raise, opportunities]
    # equity 按 (街, 动作) 分桶（翻前=vs-random 标度，翻后=vs-range 标度，不可混看）。
    eq_when = defaultdict(list)  # (street, action) -> [equity...]
    # 面对下注的弃/跟，按街收 EV 余量样本：surplus = equity - required_eq（实际价位 break-even）。
    # 翻前弃牌用 vs-random equity（虚高），不参与「弃太紧」判定，单列以免误导（详见 flag 段）。
    facing = defaultdict(list)  # (street, action) -> [(surplus, equity, required_eq, req, resp)...]

    # --- Phase A 节点 trace 聚合（旧日志无 trace → 全部保持空，优雅降级）---
    trace_n = 0                          # 带 trace 的决策数
    node_n = Counter()                   # 节点 id（含路径前缀）→ 命中决策数
    node_actions = defaultdict(Counter)  # 节点 id → 动作分布
    hand_nodes = {}                      # hand_id → 该手命中的节点集（EV 归因，§8②）
    hand_bb = {}                         # hand_id → big_blind（chips → bb 换算）
    hand_net = {}                        # hand_id → 整手 net（chips，来自 hand_result 行）
    guard_hits = 0                       # 落 fallback 或 legalize 降级的决策数（兜底率分子，§8④）
    legalized_n = 0                      # 其中 legalize 降级数

    seen_hands = set()

    for path in paths:
        try:
            f = open(path)
        except OSError as e:
            print(f"跳过 {path}: {e}", file=sys.stderr)
            continue
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                o = json.loads(line)
            except Exception:
                continue
            # hand_result 行（无 req/resp）：记整手 net，供 Phase A 节点 EV 归因。
            if o.get("kind") == "hand_result":
                hid = o.get("hand_id")
                net = o.get("net")
                if hid is not None and isinstance(net, (int, float)):
                    hand_net[hid] = net
                continue
            req = o.get("req")
            resp = o.get("resp")
            if not req or not resp:
                continue
            total += 1
            street = resp.get("street") or _street_from_board(req.get("board", []))
            by_street[street] += 1
            act = resp.get("action", "?")
            action_by_street[street][act] += 1
            src = str(resp.get("source", ""))
            sources[src.split(":")[0] if ":" in src else src] += 1
            if src.startswith("fallback"):
                fallback += 1
                fallback_reasons[src[:60]] += 1
            # --- Phase A 节点 trace（旧日志 resp 无 trace → 本块整体跳过）---
            tr = resp.get("trace")
            tr = tr if isinstance(tr, dict) else None
            was_legalized = bool(tr and tr.get("legalized"))
            if was_legalized:
                legalized_n += 1
            if src.startswith("fallback") or was_legalized:
                guard_hits += 1
            path = [p for p in (tr.get("path") or []) if isinstance(p, str)] if tr else []
            if path:
                trace_n += 1
                for node in path:  # 含前缀节点（§3.2 规则 3：路径前缀聚合可对齐拆分前后）
                    node_n[node] += 1
                    node_actions[node][act] += 1
                hand_id = (o.get("sent") or {}).get("hand_id")
                if hand_id is not None:
                    hand_nodes.setdefault(hand_id, set()).update(path)
                    hand_bb.setdefault(hand_id, max(int(req.get("big_blind") or 20), 1))
            # 激进度。
            if act == "raise":
                af_bets += 1
                if street != "preflop":
                    af_bets_post += 1
            elif act == "call":
                af_calls += 1
                if street != "preflop":
                    af_calls_post += 1
            # equity 分桶（按街拆，避免翻前 vs-random 与翻后 vs-range 混看误导）。
            eqv = resp.get("equity")
            if isinstance(eqv, (int, float)):
                eq_when[(street, act)].append(eqv)

            # 面对下注的弃/跟：算实际价位 break-even（required_eq），收 EV 余量样本（按街判过松/过紧）。
            valid = req.get("valid", {}) or {}
            cant_check = not valid.get("can_check")
            if act in ("fold", "call") and cant_check and isinstance(eqv, (int, float)):
                m = pot_metrics(req, resp)
                # 重建漂移护栏：不能 check（确实面对下注）却重建出 to_call=0 → required_eq 不可信
                #（OpenPoker 扁平历史分街偏移导致，引擎自身也对此回退 0.33）。这类样本排除出
                # EV 余量判定，否则 required_eq=0 会把任何正 equity 误判成「弃了/跟了 +EV 价位」。
                if (m is not None and isinstance(m.get("required_eq"), (int, float))
                        and m.get("to_call", 0) and m["to_call"] > 0):
                    req_eq = float(m["required_eq"])
                    surplus = eqv - req_eq
                    facing[(street, act)].append((surplus, eqv, req_eq, req, resp))

            # 翻前位置 / VPIP / PFR。
            if street == "preflop":
                n = req.get("num_seats", 9)
                button = req.get("button_seat", 0)
                my = req.get("my_seat", 0)
                pb = pos_bucket(button, my, n)
                nr = preflop_num_raises(req.get("actions", []))
                pre_decisions += 1
                if act in ("call", "raise"):
                    pre_vpip += 1
                if act == "raise":
                    pre_pfr += 1
                # open 机会 = 翻前无人加注时。
                if nr == 0:
                    open_by_pos[pb][1] += 1
                    if act == "raise":
                        open_by_pos[pb][0] += 1
        f.close()

    if total == 0:
        print("无可解析决策。")
        return

    print("=" * 60)
    print(f"启发式 9-max 决策分析 | 文件 {len(paths)} | 决策 {total}")
    print("=" * 60)

    print("\n[街分布]")
    for st in ("preflop", "flop", "turn", "river"):
        c = by_street.get(st, 0)
        print(f"  {st:8s} {c:6d} ({100.0*c/total:5.1f}%)")

    print("\n[动作频率 按街]")
    for st in ("preflop", "flop", "turn", "river"):
        d = action_by_street.get(st)
        if not d:
            continue
        tot = sum(d.values())
        parts = " ".join(f"{a}={100.0*n/tot:.0f}%" for a, n in d.most_common())
        print(f"  {st:8s} {parts}")

    print("\n[source top]")
    for s, n in sources.most_common(12):
        print(f"  {s:24s} {n:6d} ({100.0*n/total:5.1f}%)")

    print(f"\n[fallback] {fallback} ({100.0*fallback/total:.2f}%)")
    for r, n in fallback_reasons.most_common(6):
        print(f"  {n:5d}× {r}")

    af = af_bets / af_calls if af_calls else float("inf")
    afp = af_bets_post / af_calls_post if af_calls_post else float("inf")
    print(f"\n[激进度 AF] 总 {af:.2f} (bet/raise {af_bets} / call {af_calls})"
          f" | 翻后 {afp:.2f}")

    if pre_decisions:
        print(f"\n[翻前] 决策 {pre_decisions} | VPIP {100.0*pre_vpip/pre_decisions:.1f}%"
              f" | PFR {100.0*pre_pfr/pre_decisions:.1f}%")
        print("  按位置 open 率（无人加注时）:")
        for pb in ("EARLY", "MID", "LATE", "SB", "BB"):
            o, opp = open_by_pos.get(pb, [0, 0])
            if opp:
                print(f"    {pb:6s} {100.0*o/opp:5.1f}%  ({o}/{opp})")

    # equity 按街拆开：翻前=vs-random 强度（虚高，不可与翻后比），翻后=vs-range 蒙特卡洛。
    print("\n[equity 均值 按街×动作]  (翻前=vs-random 标度, 翻后=vs-range 标度，不可跨街比)")
    for st in ("preflop", "flop", "turn", "river"):
        rows = []
        for a in ("fold", "check", "call", "raise"):
            v = eq_when.get((st, a), [])
            if v:
                rows.append(f"{a}: n={len(v)} eq均值={sum(v)/len(v):.3f}"
                            f" [{min(v):.2f},{max(v):.2f}]")
        if rows:
            print(f"  {st:8s} " + " | ".join(rows))

    # required_eq 余量分布（面对下注的弃/跟，按街）：surplus = equity - 实际价位 break-even。
    print("\n[面对下注 EV 余量 surplus=equity-required_eq 按街×动作]  (margin=±%.2f)" % EV_MARGIN)
    for st in ("preflop", "flop", "turn", "river"):
        for a in ("fold", "call"):
            samp = facing.get((st, a), [])
            if not samp:
                continue
            surs = [x[0] for x in samp]
            reqs = [x[2] for x in samp]
            print(f"  {st:8s} {a:5s} n={len(samp):5d}"
                  f" required_eq均值={sum(reqs)/len(reqs):.3f}"
                  f" surplus均值={sum(surs)/len(surs):+.3f}"
                  f" [{min(surs):+.2f},{max(surs):+.2f}]")

    # --- Phase A 节点聚合（docs/scenario_heuristic_design.md §6 / §8②④）---
    if trace_n:
        # 每节点 EV 样本（bb/hand）：整手 net 摊给该手命中的所有节点（§8②，每手每节点 1 样本）。
        node_ev = defaultdict(list)
        for hid, nodes in hand_nodes.items():
            if hid not in hand_net:
                continue  # 无 hand_result（截断/丢失）→ 不参与 EV，仍计热度
            ev_bb = hand_net[hid] / hand_bb.get(hid, 20)
            for nd in nodes:
                node_ev[nd].append(ev_bb)
        print(f"\n[节点聚合 Phase A trace] 带trace决策 {trace_n}/{total}"
              f" | EV 口径=整手 net 摊给该手命中的所有节点（§8②）"
              f" | h<{NODE_MIN_HANDS} 标待观察（§4.3）")
        print(f"  {'节点':38s} {'n':>6s} {'hands':>6s} {'EV(bb/hand)':>14s} {'CI95':>8s}  动作分布")
        for node, n in sorted(node_n.items(), key=lambda kv: (-kv[1], kv[0])):
            evs = node_ev.get(node, [])
            if evs:
                m = sum(evs) / len(evs)
                sd = math.sqrt(sum((x - m) ** 2 for x in evs) / len(evs))
                ci = 1.96 * sd / math.sqrt(len(evs))
                ev_s, ci_s = f"{m:+.2f}", f"±{ci:.2f}"
                mark = "" if len(evs) >= NODE_MIN_HANDS else "  ⚠待观察"
            else:
                ev_s, ci_s, mark = "n/a", "", "  ⚠待观察"
            acts = node_actions[node]
            tot_a = sum(acts.values()) or 1
            dist = " ".join(f"{a}={100.0 * c / tot_a:.0f}%" for a, c in acts.most_common())
            print(f"  {node:38s} {n:6d} {len(evs):6d} {ev_s:>14s} {ci_s:>8s}  {dist}{mark}")
        print(f"\n[兜底率 §8④] fallback/legalize降级决策 {guard_hits}/{total}"
              f" ({100.0 * guard_hits / total:.2f}%)"
              f"  (fallback={fallback}, legalize降级={legalized_n})")
    else:
        print("\n[节点聚合 Phase A trace] 日志无 trace 字段（旧日志）→ 跳过节点聚合/兜底率")

    # 自动漏洞标记。
    print("\n[漏洞标记]")
    flags = []
    fb_rate = 100.0 * fallback / total
    if fb_rate > 2.0:
        flags.append(f"fallback 率 {fb_rate:.1f}% 偏高（>2%）→ 查重建/合法集兜底原因")
    if pre_decisions:
        vpip = 100.0 * pre_vpip / pre_decisions
        if vpip > 42:
            flags.append(f"翻前 VPIP {vpip:.0f}% 偏松（>42%）→ 收紧 open/call 阈值")
        if vpip < 16:
            flags.append(f"翻前 VPIP {vpip:.0f}% 偏紧（<16%）→ 放宽 open/偷盲")

    # --- 跟注过松：只在确实跟在 -EV 价位（equity < required_eq - margin）才报，按街分桶。---
    # required_eq 来自实际重建底池 → 多人稀释/盈亏赔率自动吸收，不再拿 raw equity 比固定常数。
    # 翻前 equity 是 vs-random（虚高），在此偏宽松（少报漏报），不会误伤；翻后是 vs-range 口径，可靠。
    for st in ("preflop", "flop", "turn", "river"):
        calls = facing.get((st, "call"), [])
        if len(calls) < 20:
            continue
        loose = sum(1 for s, _e, _r, _q, _p in calls if s < -EV_MARGIN)
        if loose > 0.15 * len(calls):
            flags.append(f"{_zh_street(st)} {100.0*loose/len(calls):.0f}% 的跟注在 -EV 价位"
                         f"（equity<required_eq-{EV_MARGIN:.2f}, {loose}/{len(calls)}）→ 跟太松")

    # --- 弃牌过紧：只在确实弃了 +EV 价位（equity > required_eq + margin）才报。---
    # **仅翻后**：翻前 equity 是 vs-random 虚高（K5o vs-random≈0.5 但面对加注是标准弃牌），
    # 拿它判「弃了好价位」必伪报；翻后 equity 已是 vs-range，与 required_eq 同口径，可比。
    for st in ("flop", "turn", "river"):
        folds = facing.get((st, "fold"), [])
        if len(folds) < 20:
            continue
        tight = sum(1 for s, _e, _r, _q, _p in folds if s > EV_MARGIN)
        if tight > 0.12 * len(folds):
            flags.append(f"{_zh_street(st)} {100.0*tight/len(folds):.0f}% 的弃牌弃了 +EV 价位"
                         f"（equity>required_eq+{EV_MARGIN:.2f}, {tight}/{len(folds)}）→ 弃太紧")

    if afp != float("inf") and afp < 0.8:
        flags.append(f"翻后 AF {afp:.2f} 偏被动（<0.8）→ 加价值下注/c-bet 频率")
    if not flags:
        print("  （无显著漏洞标记）")
    for fl in flags:
        print(f"  ⚠ {fl}")


def _zh_street(st):
    return {"preflop": "翻前", "flop": "翻牌", "turn": "转牌", "river": "河牌"}.get(st, st)


def _street_from_board(board):
    return {0: "preflop", 3: "flop", 4: "turn", 5: "river"}.get(len(board or []), "preflop")


if __name__ == "__main__":
    main()

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
  - equity 分桶（下注/跟注/弃牌时的均值 → 抓「弃了 +equity / 跟了 -equity」）
  - 自动漏洞标记（fallback 偏高、过松/过紧、激进度异常…）

用法：
  python3 tools/heuristic9_analyze.py heuristic9_actions.jsonl
  python3 tools/heuristic9_analyze.py openpoker9_actions_*.jsonl   # 也吃旧 6-max 格式
"""
import json
import sys
from collections import Counter, defaultdict

_RANK_ORD = {r: i for i, r in enumerate("23456789TJQKA")}
PREMIUM = {"AA", "KK", "QQ", "JJ", "AK"}


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
    if act == "call" and eqf is not None and eqf < 0.12 and _opps_in_pot(req.get("actions", [])) <= 1:
        out.append(("CALL_NEAR_ZERO_EQ", f"{street} HU eq={eqf:.2f}"))
    return out


def _opps_in_pot(actions):
    """据动作流估「仍在池中的对手数」= 各座最后动作非 fold 的座位数（不含 hero 自己未必在表中）。"""
    last = {}
    for a in actions:
        s = a.get("seat")
        if s is not None:
            last[s] = a.get("action")
    return sum(1 for v in last.values() if v != "fold")


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
    eq_when = defaultdict(list)  # action -> [equity...]

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
            # 激进度。
            if act == "raise":
                af_bets += 1
                if street != "preflop":
                    af_bets_post += 1
            elif act == "call":
                af_calls += 1
                if street != "preflop":
                    af_calls_post += 1
            # equity 分桶。
            eqv = resp.get("equity")
            if isinstance(eqv, (int, float)):
                eq_when[act].append(eqv)

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

    print("\n[equity 均值 按动作]")
    for a in ("fold", "check", "call", "raise"):
        v = eq_when.get(a, [])
        if v:
            print(f"  {a:6s} n={len(v):6d} eq均值={sum(v)/len(v):.3f}"
                  f" min={min(v):.2f} max={max(v):.2f}")

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
    folds = eq_when.get("fold", [])
    if folds:
        hi_eq_folds = sum(1 for e in folds if e > 0.5)
        if hi_eq_folds > 0.15 * len(folds):
            flags.append(f"{100.0*hi_eq_folds/len(folds):.0f}% 的弃牌 equity>0.5 "
                         f"→ 可能在好赔率/强牌点弃太多")
    calls = eq_when.get("call", [])
    if calls:
        lo_eq_calls = sum(1 for e in calls if e < 0.25)
        if lo_eq_calls > 0.20 * len(calls):
            flags.append(f"{100.0*lo_eq_calls/len(calls):.0f}% 的跟注 equity<0.25 "
                         f"→ 可能在差点跟太多（漏斗给跟注站）")
    if afp != float("inf") and afp < 0.8:
        flags.append(f"翻后 AF {afp:.2f} 偏被动（<0.8）→ 加价值下注/c-bet 频率")
    if not flags:
        print("  （无显著漏洞标记）")
    for fl in flags:
        print(f"  ⚠ {fl}")


def _street_from_board(board):
    return {0: "preflop", 3: "flop", 4: "turn", 5: "river"}.get(len(board or []), "preflop")


if __name__ == "__main__":
    main()

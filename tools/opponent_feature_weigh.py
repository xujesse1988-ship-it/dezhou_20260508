#!/usr/bin/env python3
"""对手画像特征「样本称重」——量第 1-2 档特征的机会数（分母）分布 + field 先验。

配套 `docs/opponent_features_inventory.md`：那份清单列了「能采哪些对手特征」，本脚本回答
「每个特征对真实对手到底有多少有效样本」，据此判定哪些现在就能按人个体化、哪些只能靠群体先验。

重放 live raw 日志（`h9_live_run*.raw.jsonl`），对每手重建动作序列，逐特征累计 per-name 的
（机会数, 命中数），再按「≥N 手对手」汇总样本分布 + 场均命中率（＝冷启动 field 先验）。

数据源纪律（重要）：动作序列取自 **`hand_result.actions`**（完整权威、含 street），**不是**
`player_action` 事件流——后者会漏「结束手的最后一个动作」（尤其 opener 对 3bet 的 fold），
用它重放会让 fold-to-3bet / fold-to-cbet 分母系统性低估近半。seat→name 由 `player_action` +
`table_state` 补（实测覆盖 100%）。

口径：raise-type = {raise, bet, all_in, allin, all-in}；翻前第 1 个 raise=open、第 2=3bet、第 3=4bet。
- 3bet_opp : 面对「恰好 1 个 open」且自己非 opener，首次决策 → 机会；raise→命中。
- ftt3_opp : opener 面对一个 3bet（链上 raises==2）→ 机会；fold→命中。
- 4bet_opp : 面对站着的 3bet（raises==2）可加注 → 机会；raise→命中。
- cbet_opp : 翻前最后加注方在 flop 被 check 到、有决策 → 机会；bet→命中。
- ftc_opp  : cbet 后其后行动者首次面对该注 → 机会；fold→命中。
- db_opp   : agg 已 cbet flop 且 turn 被 check 到、有决策 → 机会；bet→命中。
- steal/bdef: 位置从 dealer+活跃座位近似推（BTN=dealer, SB/BB/CO 顺推），数值仅量级参考。
hands 计数：该 name 在该手 `hand_result.actions` 中有动作即 +1（≈ 现有 n_preflop 口径，漏少数走盲）。

用法：
  python3 tools/opponent_feature_weigh.py                        # 默认吃 cwd 下 h9_live_run24-30.raw.jsonl
  python3 tools/opponent_feature_weigh.py 'h9_live_run*.raw.jsonl'
  python3 tools/opponent_feature_weigh.py --min-hands 150 --profile h9_live_prof.json
"""
import argparse
import glob
import json
import statistics
from collections import defaultdict

RAISE = {"raise", "bet", "all_in", "allin", "all-in"}
HERO = {"heuristic_cc"}  # 我方 bot，排除出对手集
FEATS = ["3bet", "ftt3", "4bet", "cbet", "ftc", "db", "steal", "bdef"]
LABEL = {
    "3bet": "3bet(面对open)", "ftt3": "fold-to-3bet", "4bet": "4bet(面对3bet)",
    "cbet": "flop cbet", "ftc": "fold-to-cbet", "db": "double barrel",
    "steal": "偷盲(近似)", "bdef": "盲位防守(近似)",
}
STEAL_POS = {"BTN", "CO", "SB"}
BLIND_POS = {"SB", "BB"}


def default_files():
    """默认吃 run24-30 的 raw 日志（本机 live 采集产物）。"""
    return sorted(glob.glob("h9_live_run2[4-9].raw.jsonl")) + sorted(glob.glob("h9_live_run30.raw.jsonl"))


def load_hands(files):
    """按 hand_id 归并事件；动作源＝hand_result.actions，name 从 player_action+table_state 补。"""
    hands = defaultdict(lambda: {"dealer": None, "names": {}, "acts": [], "shown": None})
    for f in files:
        with open(f) as fh:
            for ln in fh:
                try:
                    d = json.loads(ln)
                except Exception:
                    continue
                t, hid = d.get("type"), d.get("hand_id")
                if hid is None:
                    continue
                if t == "hand_start":
                    hands[hid]["dealer"] = d.get("dealer_seat")
                elif t == "player_action":
                    if d.get("name") and d.get("seat") is not None:
                        hands[hid]["names"][d["seat"]] = d["name"]
                elif t == "table_state":
                    for st in (d.get("seats") or []):
                        if isinstance(st, dict) and st.get("name") and st.get("seat") is not None:
                            hands[hid]["names"].setdefault(st["seat"], st["name"])
                elif t == "hand_result":
                    hands[hid]["acts"] = [
                        (a.get("seat"), str(a.get("action", "")).strip().lower(),
                         a.get("amount"), str(a.get("street", "")).strip().lower())
                        for a in d.get("actions", [])
                    ]
                    if d.get("shown_cards"):
                        hands[hid]["shown"] = d["shown_cards"]
    return hands


def positions(dealer, active):
    """近似位置：active=有动作的座位集合。返回 {seat: pos_label}。"""
    if dealer is None or not active:
        return {}
    seats = sorted(set(active) | {dealer})
    if dealer not in seats:
        return {}
    i = seats.index(dealer)
    order = seats[i:] + seats[:i]  # order[0]=BTN
    n = len(order)
    pos = {}
    if n == 2:
        pos[order[0]] = "BTN"  # HU：按钮=SB=BTN
        pos[order[1]] = "BB"
    else:
        pos[order[0]] = "BTN"
        pos[order[1]] = "SB"
        pos[order[2]] = "BB"
        if n >= 4:
            pos[order[-1]] = "CO"  # 按钮前一位
    return pos


def replay(hands, stat):
    """逐手重放，累计 per-name 的 (机会, 命中) 到 stat。"""
    for h in hands.values():
        acts = h["acts"]
        if not acts:
            continue
        nm_of = h["names"]
        name_of = nm_of.get

        actors = {name_of(s) for s, *_ in acts if name_of(s)}
        for nm in actors:
            stat[nm]["hands"] += 1
        if h["shown"]:
            for k in h["shown"]:
                try:
                    nm = name_of(int(k))
                except Exception:
                    nm = None
                if nm:
                    stat[nm]["shown"] += 1

        active = {s for s, *_ in acts if name_of(s)}
        pos = positions(h["dealer"], active)

        # ---- 翻前 ----
        pre = [a for a in acts if a[3] == "preflop"]
        raises = 0
        opener = None
        last_raiser = None
        vol_before = False
        c3 = set(); cftt3 = set(); c4 = set(); cst = set(); cbd = set()
        for seat, act, _amt, _ in pre:
            name = name_of(seat)
            if not name:
                continue
            p = pos.get(seat)
            if raises == 1 and seat != opener and seat not in c3:
                stat[name]["3bet"][0] += 1
                if act in RAISE:
                    stat[name]["3bet"][1] += 1
                c3.add(seat)
            if raises == 2:
                if seat not in c4:
                    stat[name]["4bet"][0] += 1
                    if act in RAISE:
                        stat[name]["4bet"][1] += 1
                    c4.add(seat)
                if seat == opener and seat not in cftt3:
                    stat[name]["ftt3"][0] += 1
                    if act == "fold":
                        stat[name]["ftt3"][1] += 1
                    cftt3.add(seat)
            if raises == 0 and not vol_before and p in STEAL_POS and seat not in cst:
                stat[name]["steal"][0] += 1
                if act in RAISE:
                    stat[name]["steal"][1] += 1
                cst.add(seat)
            if raises == 1 and p in BLIND_POS and pos.get(opener) in STEAL_POS and seat not in cbd:
                stat[name]["bdef"][0] += 1
                if act != "fold":
                    stat[name]["bdef"][1] += 1
                cbd.add(seat)
            if act in RAISE:
                if raises == 0:
                    opener = seat
                raises += 1
                last_raiser = seat
            elif act == "call":
                vol_before = True

        agg = last_raiser  # 翻前最后加注方
        # ---- flop cbet + fold-to-cbet ----
        flop = [a for a in acts if a[3] == "flop"]
        cbet_done = False
        if agg is not None and flop:
            bet_before = False
            for seat, act, _amt, _ in flop:
                if seat == agg:
                    name = name_of(seat)
                    if not bet_before and name:
                        stat[name]["cbet"][0] += 1
                        if act in RAISE:
                            stat[name]["cbet"][1] += 1
                            cbet_done = True
                    break
                if act in RAISE:
                    bet_before = True
            if cbet_done:
                seen = False
                cftc = set()
                for seat, act, _amt, _ in flop:
                    if seat == agg:
                        seen = True
                        continue
                    name = name_of(seat)
                    if seen and name and seat not in cftc:
                        stat[name]["ftc"][0] += 1
                        if act == "fold":
                            stat[name]["ftc"][1] += 1
                        cftc.add(seat)
                        if act in RAISE:  # 被 raise，cbet 线终止
                            break
        # ---- turn double barrel ----
        turn = [a for a in acts if a[3] == "turn"]
        if cbet_done and agg is not None and turn:
            bet_before = False
            for seat, act, _amt, _ in turn:
                if seat == agg:
                    name = name_of(seat)
                    if not bet_before and name:
                        stat[name]["db"][0] += 1
                        if act in RAISE:
                            stat[name]["db"][1] += 1
                    break
                if act in RAISE:
                    bet_before = True


def report(stat, hands, min_hands, profile_path):
    for h in HERO:
        stat.pop(h, None)
    prof = {}
    try:
        prof = json.load(open(profile_path)).get("by_name", {})
    except Exception:
        pass
    reg = sorted([nm for nm, s in stat.items() if s["hands"] >= min_hands],
                 key=lambda n: -stat[n]["hands"])
    prof_ge = sum(1 for n, s in prof.items() if s.get("hands", 0) >= min_hands and n not in HERO)
    print(f"文件去重手数:{len(hands)} | 对手名(除hero):{len(stat)}")
    print(f"≥{min_hands}手(本次日志)对手:{len(reg)}  [profile({profile_path})≥{min_hands}手:{prof_ge}]")
    if not reg:
        print("无 ≥门槛 对手，退出。")
        return
    print()
    print(f"{'特征':<20}{'总机会':>8}{'中位/人':>8}{'≥10人':>7}{'≥20人':>7}{'≥30人':>7}{'≥50人':>7}   命中率")
    print("-" * 86)
    for f in FEATS:
        opps = [stat[nm][f][0] for nm in reg]
        to = sum(stat[nm][f][0] for nm in stat)
        ta = sum(stat[nm][f][1] for nm in stat)
        med = statistics.median(opps) if opps else 0
        g = lambda k: sum(1 for x in opps if x >= k)
        rate = (ta / to * 100) if to else 0
        print(f"{LABEL[f]:<20}{to:>8}{med:>8.0f}{g(10):>7}{g(20):>7}{g(30):>7}{g(50):>7}   {rate:5.1f}%")
    print()
    print("VPIP 基线：中位手数/人 =", int(statistics.median([stat[nm]["hands"] for nm in reg])))
    sh = [stat[nm]["shown"] for nm in reg]
    print(f"摊牌真值(§D)：合计亮牌 {sum(sh)}，中位/人 {int(statistics.median(sh))}，"
          f"≥30亮牌对手 {sum(1 for x in sh if x >= 30)} 人")
    print()
    print("前 12 名对手各特征机会数：")
    print(f"{'name':<20}{'hands':>6}" + "".join(f"{f:>7}" for f in FEATS) + f"{'shown':>7}")
    for nm in reg[:12]:
        s = stat[nm]
        print(f"{nm:<20}{s['hands']:>6}" + "".join(f"{s[f][0]:>7}" for f in FEATS) + f"{s['shown']:>7}")


def main():
    ap = argparse.ArgumentParser(description="对手画像特征样本称重")
    ap.add_argument("files", nargs="*", help="raw jsonl（可 glob）；缺省吃 h9_live_run24-30.raw.jsonl")
    ap.add_argument("--min-hands", type=int, default=100, help="纳入汇总的最小手数（默认 100）")
    ap.add_argument("--profile", default="h9_live_prof.json", help="跨 session profile（交叉对比用；缺失则跳过）")
    args = ap.parse_args()

    patterns = args.files or None
    files = []
    if patterns:
        for p in patterns:
            files.extend(sorted(glob.glob(p)))
    else:
        files = default_files()
    if not files:
        raise SystemExit("未找到输入日志（默认 h9_live_run24-30.raw.jsonl）。传 glob 参数或切到日志目录。")

    hands = load_hands(files)
    stat = defaultdict(lambda: {"hands": 0, "shown": 0, **{f: [0, 0] for f in FEATS}})
    replay(hands, stat)
    report(stat, hands, args.min_hands, args.profile)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Match report for a tools/wopo_arena run, read only from its logs.

Statistics use the round (one duplicate deal, n hands) as the sample: within a round every bot plays
the same cards from the same positions, so the card luck that cancels out stays out of the error bars.
"""

import argparse
import collections
import json
import math
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(os.path.dirname(HERE)))

from opponent_analysis.adapters import wopo_raw  # noqa: E402
from opponent_analysis.platforms import PLATFORMS  # noqa: E402
from opponent_analysis.replay import ReplayError, replay  # noqa: E402

POSITIONS_8 = ("SB", "BB", "UTG", "UTG1", "LJ", "HJ", "CO", "BTN")
RAKE_PCT, RAKE_CAP_BB = 4, 10
BIG_POT_BB, BIG_POT_PCT = 200, 1.5


def read_jsonl(path):
    if not os.path.exists(path):
        return
    with open(path, encoding="utf-8") as f:
        for line in f:
            if line.strip():
                yield json.loads(line)


def mean_ci(values):
    """(mean, half width of the 95% interval)."""
    n = len(values)
    if n == 0:
        return 0.0, 0.0
    m = sum(values) / n
    if n < 2:
        return m, float("inf")
    var = sum((x - m) ** 2 for x in values) / (n - 1)
    return m, 1.96 * math.sqrt(var / n)


def group_of(name):
    return name.rsplit("_", 1)[0]


def analyse(out_dir):
    with open(os.path.join(out_dir, "run.json")) as f:
        run = json.load(f)
    hands = list(read_jsonl(os.path.join(out_dir, "table", "wopo_hh.jsonl")))
    seats = run["config"]["table"]["seats"]
    bb = run["config"]["table"]["big_blind"]
    groups = sorted({group_of(n) for h in hands for n in h["names"].values()})
    copies = collections.Counter(group_of(n) for n in (hands[0]["names"].values() if hands else []))

    per_round = collections.defaultdict(lambda: collections.Counter())
    per_hand = collections.defaultdict(list)
    per_round_rake = collections.defaultdict(lambda: collections.Counter())
    by_pos = collections.defaultdict(lambda: collections.Counter())
    by_pos_n = collections.Counter()
    stats = collections.defaultdict(collections.Counter)
    resets = collections.defaultdict(collections.Counter)
    problems = collections.Counter()
    streets = collections.Counter()

    for h in hands:
        res = h["result"]
        names = {int(s): n for s, n in h["names"].items()}
        start = {int(s): v for s, v in h["initial_stacks"].items()}
        final = {int(s): v for s, v in res["final_stacks"].items()}
        if sum(final.values()) != sum(start.values()):
            problems["chips_not_conserved"] += 1
        try:
            replay(wopo_raw(h, "arena"), PLATFORMS["wopo"].heads_up_button_is_sb)
        except (ReplayError, KeyError, TypeError, ValueError) as e:
            problems[f"replay:{e}"] += 1
        n = len(names)
        # Same model as RakeCfg::wopo in tools/wopo_exploit/src/game.rs; pots ending preflop are raked too.
        rake = min(res["total_pot"] * RAKE_PCT / 100, RAKE_CAP_BB * bb)
        if res["total_pot"] >= BIG_POT_BB * bb:
            rake += res["total_pot"] * BIG_POT_PCT / 100
        paid = {p["seat"]: p["amount"] for p in res["payouts"]}
        total_paid = sum(paid.values()) or 1
        hand_net = collections.Counter()
        for s, name in names.items():
            hand_net[group_of(name)] += final[s] - start[s]
        for g, v in hand_net.items():
            per_hand[g].append(v)
        for s, name in names.items():
            g = group_of(name)
            net = final[s] - start[s]
            per_round[h["round_no"]][g] += net
            per_round_rake[h["round_no"]][g] += net - rake * paid.get(s, 0) / total_paid
            pos = (s - h["button_seat"] - 1) % n
            by_pos[g][pos] += net
            by_pos_n[(g, pos)] += 1
        for name, r in (h.get("stack_resets") or {}).items():
            kind = "rebuy" if r["to"] > r["from"] else "reset"
            resets[group_of(name)][kind] += 1
        seen = set()
        raised = set()
        for a in h["actions"]:
            g = group_of(names[a["seat"]])
            if a["street"] == "preflop":
                if a["seat"] not in seen:
                    seen.add(a["seat"])
                    stats[g]["pre_hands"] += 1
                if a["action"] in ("call", "raise", "all_in"):
                    raised.add((a["seat"], "vpip"))
                if a["action"] in ("raise", "all_in"):
                    raised.add((a["seat"], "pfr"))
        for s, kind in raised:
            stats[group_of(names[s])][kind] += 1
        if h["board"]:
            streets["saw_flop"] += 1
        live = n - sum(1 for a in h["actions"] if a["action"] == "fold")
        if live >= 2:
            streets["showdown"] += 1
            for s in names:
                if not any(a["seat"] == s and a["action"] == "fold" for a in h["actions"]):
                    stats[group_of(names[s])]["showdown"] += 1
                    stats[group_of(names[s])]["showdown_net"] += final[s] - start[s]

    sources = collections.defaultdict(collections.Counter)
    decide_ms = collections.defaultdict(list)
    seat_to_name = {}
    for h in hands:
        seat_to_name[h["hand_id"]] = {int(s): n for s, n in h["names"].items()}
    notes = collections.Counter()
    for e in read_jsonl(os.path.join(out_dir, "table", "wopo_play_events.jsonl")):
        if e.get("type") == "decision" and e["hand_id"] in seat_to_name:
            g = group_of(seat_to_name[e["hand_id"]][e["seat"]])
            src = (e.get("source") or "?").split(":")[0]
            sources[g][src] += 1
            decide_ms[g].append(e.get("decide_ms") or 0)
        # Hands cut off by a restart are not in the record and are played again; their events do not count.
        if e.get("type") == "hero_action" and e.get("note") and e["hand_id"] in seat_to_name:
            notes[e["note"]] += 1

    # Each driver re-checks every message, so its counts are the cross-check of the table's rules.
    drivers = {}
    for name in sorted(os.listdir(out_dir)):
        d = os.path.join(out_dir, name)
        if name == "table" or not os.path.exists(os.path.join(d, "driver.pid")):
            continue
        kinds = collections.Counter()
        fallbacks = collections.Counter()
        for e in read_jsonl(os.path.join(d, "wopo_play_events.jsonl")):
            if e.get("type") == "dead":
                kinds[e.get("tag") or "dead"] += 1
            if e.get("type") == "result" and e.get("notes"):
                kinds["result_notes"] += 1
            if e.get("type") == "decision":
                if e.get("stale"):
                    kinds["stale"] += 1
                src = e.get("source") or ""
                if src.startswith("fallback"):
                    fallbacks[":".join(src.split(":")[1:2]) or "fallback"] += 1
        hh = sum(1 for _ in read_jsonl(os.path.join(d, "wopo_hh.jsonl")))
        drivers[name] = {"hh": hh, "kinds": kinds, "fallbacks": fallbacks}

    return {"run": run, "hands": hands, "seats": seats, "bb": bb, "groups": groups, "copies": copies,
            "per_round": per_round, "per_hand": per_hand, "per_round_rake": per_round_rake, "by_pos": by_pos, "by_pos_n": by_pos_n,
            "stats": stats, "resets": resets, "problems": problems, "streets": streets, "sources": sources,
            "decide_ms": decide_ms, "drivers": drivers, "notes": notes}


def bb100(values, hands_per_round, bb, copies):
    """Per-round chips of one group -> bb/100 per seat with a 95% half width."""
    scale = 100 / (hands_per_round * bb * copies)
    m, ci = mean_ci(values)
    return m * scale, ci * scale


def format_report(a):
    hands, seats, bb, groups = a["hands"], a["seats"], a["bb"], a["groups"]
    rounds = sorted(a["per_round"])
    full = [r for r in rounds if sum(1 for h in hands if h["round_no"] == r) == seats]
    out = [f"手数 {len(hands)}，完整轮 {len(full)}（每轮 {seats} 手），commit {a['run'].get('commit')}，seed {a['run'].get('seed')}", ""]

    out.append("## 每座盈亏（bb/100，按完整轮算的 95% 区间）")
    out.append("| bot | 不抽水 | 按 wopo 抽水模型算 | 不按轮配对时的区间 | 总盈亏(BB，两座合计) | 补码 | 超线重进 |")
    out.append("|---|---|---|---|---|---|---|")
    rows = []
    for g in groups:
        vals = [a["per_round"][r][g] for r in full]
        rvals = [a["per_round_rake"][r][g] for r in full]
        m, ci = bb100(vals, seats, bb, a["copies"][g])
        rm, rci = bb100(rvals, seats, bb, a["copies"][g])
        total = sum(a["per_round"][r][g] for r in rounds) / bb
        _, naive = bb100(a["per_hand"][g], 1, bb, a["copies"][g])
        rows.append((m, f"| {g} | {m:+.1f} ± {ci:.1f} | {rm:+.1f} ± {rci:.1f} | ± {naive:.1f} | {total:+.0f} | "
                        f"{a['resets'][g]['rebuy']} | {a['resets'][g]['reset']} |"))
    out.extend(r[1] for r in sorted(rows, key=lambda x: -x[0]))

    out += ["", "## 两两差值（行 − 列，bb/100 每座，同一轮配对）"]
    out.append("| |" + "|".join(groups) + "|")
    out.append("|---|" + "---|" * len(groups))
    for x in groups:
        cells = []
        for y in groups:
            if x == y:
                cells.append("")
                continue
            vals = [a["per_round"][r][x] / a["copies"][x] - a["per_round"][r][y] / a["copies"][y] for r in full]
            m, ci = bb100(vals, seats, bb, 1)
            cells.append(f"{m:+.0f} ± {ci:.0f}")
        out.append(f"| {x} |" + "|".join(cells) + "|")

    if seats == 8:
        out += ["", "## 各位置盈亏（BB/手）", "| bot |" + "|".join(POSITIONS_8) + "|", "|---|" + "---|" * 8]
        for g in groups:
            cells = [f"{a['by_pos'][g][p] / bb / max(a['by_pos_n'][(g, p)], 1):+.1f}" for p in range(8)]
            out.append(f"| {g} |" + "|".join(cells) + "|")

    out += ["", "## 打法", "| bot | 入池 | 翻前加注 | 摊牌手数 | 摊牌盈亏(BB) | 决策来源 | 决策耗时中位/最大(ms) |", "|---|---|---|---|---|---|---|"]
    for g in groups:
        s = a["stats"][g]
        n = max(s["pre_hands"], 1)
        src = " ".join(f"{k}:{v}" for k, v in a["sources"][g].most_common(6))
        ms = sorted(a["decide_ms"][g]) or [0]
        out.append(f"| {g} | {100 * s['vpip'] / n:.1f}% | {100 * s['pfr'] / n:.1f}% | {s['showdown']} | "
                   f"{s['showdown_net'] / bb:+.0f} | {src} | {ms[len(ms) // 2]} / {ms[-1]} |")

    out += ["", "## 校验"]
    out.append(f"- 牌桌牌谱复核（筹码守恒 + opponent_analysis 重放）：{dict(a['problems']) or '全部通过'}")
    out.append(f"- 牌桌层修正动作：{dict(a['notes']) or '无'}")
    expected = len(hands)
    for name, d in a["drivers"].items():
        flag = "" if d["hh"] >= expected and not d["kinds"] else " ←"
        fb = f"，advisor 兜底决策 {dict(d['fallbacks'])}" if d["fallbacks"] else ""
        out.append(f"- {name}: hh {d['hh']}/{expected}，作废或结算对不上 {dict(d['kinds']) or '无'}{flag}{fb}")
    return "\n".join(out)


def main():
    p = argparse.ArgumentParser(description="wopo_arena 对打报告")
    p.add_argument("out", help="arena.py --out 目录")
    args = p.parse_args()
    print(format_report(analyse(args.out)))


if __name__ == "__main__":
    main()

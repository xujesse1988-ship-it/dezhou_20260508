#!/usr/bin/env python3
"""M0 基线阶梯 + 评估 harness：对手 169 类 range 预测。

基线（docs/opponent_range_ml_design.md §7）：
  B0        均匀 × card-removal（绝对地板）
  B1        人群频率先验：P(y | 翻前角色 × 位置桶)，train 切分统计 + Dirichlet 平滑
  B2a       现状标量 tracker：build_opp_thresholds（policy.rs:53）逐街复刻 → 硬阈值 range
  B2b-prior multi_field §2.2 软化角色模板先验（无翻后算子）
  B2b       §2 完整 RangeVec：模板先验 + 逐动作贝叶斯似然算子（L_bet/L_raise/L_call/L_check）
            —— **M0 出闸数字 = B2b 分街 top-5 命中率**（ML 必须打赢它才立项接线）

评估：4 个街末快照（翻前/翻牌/转牌/河牌末，同一批摊牌手 → 分街数即匹配队列的单调性
检验）。指标：top-1/5/10 命中、NLL、label 排名（均值/中位）、label 质量、label 被硬过滤
排除率（B2a 专属——现状引擎「board-blind 硬过滤」的直接控诉指标）。

card removal 口径（全基线统一）：mass[k] ∝ shape[k] × alive_k/total_k，alive 按 hero
视角死牌（hero 底牌+board）计——与 §2.5「类质量不因阻断改变」的采样约定不同，这里
取 Bayes 正确版，代码内注明。
"""

import argparse
import json
import math
import os
import sys
import time
from collections import Counter, defaultdict

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from range_ml_common import (  # noqa: E402
    build_strength_table, threshold_for_top, class_axes, alive_counts, dead_mask_of,
    card_id, class_name, combo_weight, range_threshold_shift, classify,
)

AGGR = ("bet", "raise", "allin", "all_in")

# ---------------------------------------------------------------------------
# B2 参数（§2.3/§2.7 的手工默认；--b2-params JSON 可覆盖）
# ---------------------------------------------------------------------------

B2_PARAMS = {
    # 角色模板：frac → s0（threshold_for_top），cap_frac → 顶部截断（None=不截），
    # k = logistic 软度（强度尺），air = air_floor。
    "templates": {
        "3bet":     {"frac": 0.08, "cap_frac": None, "k": 0.03, "air": 0.015},
        "open":     {"frac": 0.17, "cap_frac": None, "k": 0.03, "air": 0.02},
        "coldcall": {"frac": 0.34, "cap_frac": 0.05, "k": 0.04, "air": 0.05},
        "limp":     {"frac": 0.55, "cap_frac": 0.10, "k": 0.05, "air": 0.10},
        "bbcheck":  {"frac": 0.80, "cap_frac": 0.12, "k": 0.06, "air": 0.15},
    },
    # 下注似然 L_bet = a_v·v^κ + a_d·d^κ + eps，κ(β)=κ0+κ1·min(β,1.5)。
    "bet":   {"a_v": 1.0, "a_d": 0.30, "eps": 0.05, "k0": 1.1, "k1": 1.2},
    # 加注似然（更极化）：κr = κ(β)+extra。
    "raise": {"a_v": 1.0, "a_d": 0.25, "eps": 0.03, "extra": 0.8},
    # 跟注似然（condensed）：bell(v)+听牌−坚果−空气。
    "call":  {"b_v": 1.0, "mu": 0.45, "sigma": 0.22, "b_d": 0.60,
              "nut_pen": 0.35, "air_pen": 0.55, "floor": 0.02},
    # 过牌似然：中段峰 + 放弃项。
    "check": {"c0": 0.35, "c_med": 0.50, "giveup": 0.40},
    "lik_floor": 1e-4,
}


# ---------------------------------------------------------------------------
# 样本回放：街末状态（承诺/最高注/pot/本街 aggressor）
# ---------------------------------------------------------------------------


class HandStates:
    """从统一事件流重建各街末状态，供 B2a 阈值复刻与 B2b 算子取 β。"""

    def __init__(self, s):
        self.sample = s
        self.commit = defaultdict(lambda: defaultdict(int))  # street → seat → 本街累计
        self.commit[0][s["sb"]] = s["sbamt"]
        self.commit[0][s["bb"]] = s["bbamt"]
        self.high = defaultdict(int, {0: s["bbamt"]})
        self.pot_end = {}          # street → 街末 pot
        self.aggressor = {}        # street → 本街最后激进者
        pot = s["sbamt"] + s["bbamt"]
        for e in s["ev"]:
            st = e["st"]
            self.commit[st][e["s"]] = e["c"]
            self.high[st] = max(self.high[st], e["c"])
            pot = e["p"]
            self.pot_end[st] = pot
            if e["a"] in AGGR and e["c"] > e["hb"]:
                self.aggressor[st] = e["s"]
        self.final_pot = pot

    def committed_total(self, seat, upto_street):
        return sum(self.commit[st].get(seat, 0) for st in range(upto_street + 1))

    def pot_at(self, street):
        for st in range(street, -1, -1):
            if st in self.pot_end:
                return self.pot_end[st]
        return self.sample["sbamt"] + self.sample["bbamt"]


def preflop_role(s):
    """§2.2 / policy.rs:83-98 的翻前角色（对目标座位）。"""
    tseat = s["tseat"]
    if s.get("pagg") == tseat:
        return "3bet" if s.get("npr", 0) >= 2 else "open"
    states = HandStates(s)
    committed_bb = states.commit[0].get(tseat, 0) / max(1, s["bbamt"])
    if committed_bb >= 2.5:
        return "coldcall"
    if committed_bb >= 1.5:
        return "limp"
    return "bbcheck"


ROLE_FRAC = {"3bet": 0.08, "open": 0.17, "coldcall": 0.34, "limp": 0.55, "bbcheck": 0.80}


def pos_bucket(s):
    t = s["tseat"]
    if t == s.get("btn"):
        return "BTN"
    if t == s.get("sb"):
        return "SB"
    if t == s.get("bb"):
        return "BB"
    return "MID"


# ---------------------------------------------------------------------------
# 基线实现（全部返回未归一 169 权重；harness 统一 × alive/total 后归一）
# ---------------------------------------------------------------------------


def b0_shape(_ctx):
    return [1.0] * 169  # 每 combo 等权 → shape=均匀，card removal 由 harness 施加


class B1Population:
    """P(y | 翻前角色 × 位置桶)：train 切分频次 + Dirichlet(α·combo份额) 平滑，
    键稀疏时回退 (角色) → 全局。"""

    def __init__(self, alpha=30.0, min_count=30):
        self.alpha = alpha
        self.min_count = min_count
        self.by_key = defaultdict(Counter)
        self.by_role = defaultdict(Counter)
        self.glob = Counter()

    def fit(self, samples):
        for s in samples:
            role, pos = preflop_role(s), pos_bucket(s)
            self.by_key[(role, pos)][s["y"]] += 1
            self.by_role[role][s["y"]] += 1
            self.glob[s["y"]] += 1

    def shape(self, ctx):
        s = ctx["sample"]
        role, pos = ctx["role"], pos_bucket(s)
        cnt = self.by_key[(role, pos)]
        if sum(cnt.values()) < self.min_count:
            cnt = self.by_role[role]
        if sum(cnt.values()) < self.min_count:
            cnt = self.glob
        tot = sum(cnt.values())
        out = []
        for k in range(169):
            prior_share = combo_weight(k) / 1326.0
            # shape 是 per-combo 权重：类频次平摊回 combo 级，避免与 harness 的
            # alive 乘法重复计入 combo 数。
            p_class = (cnt.get(k, 0) + self.alpha * prior_share) / (tot + self.alpha)
            out.append(p_class / combo_weight(k))
        return out


class B2Scalar:
    """现状引擎逐街复刻：thr = threshold_for_top(frac) + 0.03·street
    + [本街 aggressor] 0.12·clamp(high/pot,0,1.5) + 画像偏移 → 硬过滤。"""

    def __init__(self, strength):
        self.strength = strength
        self._thr_cache = {}

    def thr_for(self, frac):
        if frac not in self._thr_cache:
            self._thr_cache[frac] = threshold_for_top(self.strength, frac)
        return self._thr_cache[frac]

    def shape(self, ctx):
        s, S, states = ctx["sample"], ctx["street"], ctx["states"]
        tseat = s["tseat"]
        is_pre_aggr = s.get("pagg") == tseat
        is_street_aggr = states.aggressor.get(S) == tseat
        if is_pre_aggr or is_street_aggr:
            frac = 0.08 if s.get("npr", 0) >= 2 else 0.17
        else:
            committed_bb = states.committed_total(tseat, S) / max(1, s["bbamt"])
            frac = 0.34 if committed_bb >= 2.5 else (0.55 if committed_bb >= 1.5 else 0.80)
        thr = self.thr_for(frac) + 0.03 * S
        if S >= 1 and states.high[S] > 0 and is_street_aggr:
            bet_frac = min(1.5, max(0.0, states.high[S] / max(1, states.pot_at(S))))
            thr += 0.12 * bet_frac
        thr += range_threshold_shift(ctx["prof_life"])
        thr = max(0.0, min(0.96, thr))
        shape = [1.0 if self.strength[k] >= thr else 0.0 for k in range(169)]
        if not any(shape):
            shape = [1.0] * 169  # equity.rs:181 空池兜底
        return shape


def _logistic(x):
    if x < -40:
        return 0.0
    if x > 40:
        return 1.0
    return 1.0 / (1.0 + math.exp(-x))


class B2RangeVec:
    """multi_field §2 复刻：软化模板先验 + 逐动作似然算子。use_ops=False → B2b-prior。"""

    def __init__(self, strength, params=None, use_ops=True):
        self.strength = strength
        self.p = params or B2_PARAMS
        self.use_ops = use_ops
        self._s0 = {r: threshold_for_top(strength, t["frac"]) for r, t in self.p["templates"].items()}
        self._cap = {r: (threshold_for_top(strength, t["cap_frac"]) if t["cap_frac"] else None)
                     for r, t in self.p["templates"].items()}

    def prior(self, role, prof_life):
        t = self.p["templates"][role]
        s0 = self._s0[role] + range_threshold_shift(prof_life)  # §2.2：画像平移 s0
        cap_top, k = self._cap[role], t["k"]
        out = []
        for idx in range(169):
            sv = self.strength[idx]
            incl = _logistic((sv - s0) / k)
            cap = 1.0 - _logistic((sv - cap_top) / k) if cap_top is not None else 1.0
            out.append(incl * cap + t["air"])
        return out

    def op(self, ev, v, d):
        """单个目标动作的似然向量 L[169]。v/d 为该街 board 的 per-class 轴。"""
        P = self.p
        a = ev["a"]
        aggressive = a in AGGR and ev["c"] > ev["hb"]
        L = [0.0] * 169
        if aggressive:
            beta = min(1.5, ev["c"] / max(1, ev["p"]))
            if ev["hb"] == 0:
                pb = P["bet"]
                kappa = pb["k0"] + pb["k1"] * beta
                for k in range(169):
                    L[k] = pb["a_v"] * (v[k] ** kappa) + pb["a_d"] * (d[k] ** kappa) + pb["eps"]
            else:
                pr, pb = P["raise"], P["bet"]
                kappa = pb["k0"] + pb["k1"] * beta + pr["extra"]
                for k in range(169):
                    L[k] = pr["a_v"] * (v[k] ** kappa) + pr["a_d"] * (d[k] ** kappa) + pr["eps"]
        elif a == "call" or (a in AGGR and ev["c"] <= ev["hb"]):  # 短筹码 all-in 跟注归 call
            pc = P["call"]
            inv2s2 = 1.0 / (2 * pc["sigma"] * pc["sigma"])
            for k in range(169):
                x = pc["b_v"] * math.exp(-((v[k] - pc["mu"]) ** 2) * inv2s2) + pc["b_d"] * d[k]
                if v[k] > 0.9:
                    x -= pc["nut_pen"]
                if v[k] < 0.1 and d[k] < 0.15:
                    x -= pc["air_pen"]
                L[k] = max(pc["floor"], x)
        elif a == "check":
            pk = P["check"]
            for k in range(169):
                tent = max(0.0, 1.0 - 2.0 * abs(v[k] - 0.5))
                x = pk["c0"] + pk["c_med"] * tent
                if v[k] < 0.15 and d[k] < 0.2:
                    x += pk["giveup"]
                L[k] = x
        else:  # fold 或未知：不施算子
            return None
        floor = P["lik_floor"]
        return [max(floor, x) for x in L]

    def shape(self, ctx):
        shape = self.prior(ctx["role"], ctx["prof_life"])
        if not self.use_ops:
            return shape
        s, S = ctx["sample"], ctx["street"]
        for e in s["ev"]:
            if e["s"] != s["tseat"] or e["st"] == 0 or e["st"] > S:
                continue
            axes = ctx["axes_fn"](e["st"])
            if axes is None:
                continue
            L = self.op(e, axes[0], axes[1])
            if L is None:
                if e["a"] == "fold":
                    break
                continue
            shape = [m * l for m, l in zip(shape, L)]
        return shape


# ---------------------------------------------------------------------------
# 评估 harness
# ---------------------------------------------------------------------------


class AxesCache:
    def __init__(self, cap=6000):
        self.cache = {}
        self.cap = cap
        self.hits = self.misses = 0

    def get(self, board_ids, dead):
        key = (board_ids, dead)
        if key in self.cache:
            self.hits += 1
            return self.cache[key]
        self.misses += 1
        val = class_axes(list(board_ids), dead)
        if len(self.cache) < self.cap:
            self.cache[key] = val
        return val


STREET_NEED = {0: 0, 1: 3, 2: 4, 3: 5}
STREET_NAME = {0: "preflop", 1: "flop", 2: "turn", 3: "river"}


def eval_samples(samples, baselines, axes_cache, tag, limit=None):
    """返回 metrics[(baseline, street)] 累加器。"""
    met = defaultdict(lambda: {"n": 0, "top1": 0, "top5": 0, "top10": 0,
                               "nll": 0.0, "rank": 0.0, "ranks": [], "mass": 0.0, "excl": 0})
    t0 = time.time()
    n_done = 0
    for s in samples:
        if limit and n_done >= limit:
            break
        n_done += 1
        board_ids = tuple(card_id(c) for c in s["board"])
        hero_ids = tuple(card_id(c) for c in (s.get("hhole") or []))
        states = HandStates(s)
        role = preflop_role(s)
        prof_life = (s.get("prof") or {}).get("life") or {}
        y = s["y"]
        for S in (0, 1, 2, 3):
            need = STREET_NEED[S]
            if len(board_ids) < need:
                continue
            bslice = board_ids[:need]
            dead = dead_mask_of(hero_ids + bslice)
            if S == 0:
                alive = alive_counts(dead)
                axes_fn = lambda st: None  # noqa: E731
            else:
                cached = {st: axes_cache.get(board_ids[:STREET_NEED[st]],
                                             dead_mask_of(hero_ids + board_ids[:STREET_NEED[st]]))
                          for st in range(1, S + 1)}
                alive = cached[S][2]
                axes_fn = lambda st, _c=cached: (_c[st][0], _c[st][1]) if st in _c else None  # noqa: E731
            ctx = {"sample": s, "street": S, "states": states, "role": role,
                   "prof_life": prof_life, "axes_fn": axes_fn}
            for name, bl in baselines.items():
                shape = bl(ctx) if callable(bl) else bl.shape(ctx)
                # card removal（Bayes 正确版）：mass ∝ shape × alive。
                mass = [sh * al for sh, al in zip(shape, alive)]
                tot = sum(mass)
                excluded = mass[y] <= 0.0
                if tot <= 0:
                    mass = [float(al) for al in alive]
                    tot = sum(mass)
                p = [m / tot for m in mass]
                order = sorted(range(169), key=lambda k: (-p[k], k))
                rank = order.index(y)
                m = met[(name, S)]
                m["n"] += 1
                m["top1"] += rank < 1
                m["top5"] += rank < 5
                m["top10"] += rank < 10
                m["nll"] += -math.log(max(p[y], 1e-12))
                m["rank"] += rank + 1
                m["ranks"].append(rank + 1)
                m["mass"] += p[y]
                m["excl"] += excluded
    dt = time.time() - t0
    print(f"[eval] {tag}: {n_done} samples in {dt:.1f}s "
          f"(axes cache hit {axes_cache.hits}/{axes_cache.hits+axes_cache.misses})")
    return met


def fmt_table(met, baselines_order, split_name):
    lines = [f"### {split_name}", "",
             "| 基线 | 街 | top-1 | **top-5** | top-10 | NLL | rank 均值 | rank 中位 | label 质量 | 排除率 |",
             "|---|---|---|---|---|---|---|---|---|---|"]
    for name in baselines_order:
        for S in (0, 1, 2, 3):
            m = met.get((name, S))
            if not m or m["n"] == 0:
                continue
            n = m["n"]
            ranks = sorted(m["ranks"])
            med = ranks[len(ranks) // 2]
            lines.append(
                f"| {name} | {STREET_NAME[S]} | {m['top1']/n:.3f} | **{m['top5']/n:.3f}** | "
                f"{m['top10']/n:.3f} | {m['nll']/n:.3f} | {m['rank']/n:.1f} | {med} | "
                f"{m['mass']/n:.4f} | {m['excl']/n:.3f} |")
        lines.append("|  |  |  |  |  |  |  |  |  |  |")
    return lines


def monotonic_check(met, name):
    vals = [met[(name, S)]["top5"] / met[(name, S)]["n"] for S in (0, 1, 2, 3)
            if (name, S) in met and met[(name, S)]["n"]]
    ok = all(vals[i] <= vals[i + 1] + 1e-9 for i in range(len(vals) - 1))
    return vals, ok


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--ds", default="artifacts/range_ml")
    ap.add_argument("--report", default="artifacts/range_ml/m0_baseline_report.md")
    ap.add_argument("--strength-samples", type=int, default=8000)
    ap.add_argument("--train-runs", default="run24,run25,run26,run27")
    ap.add_argument("--val-runs", default="run28")
    ap.add_argument("--test-runs", default="run29,run30")
    ap.add_argument("--limit", type=int, default=0, help="每切分最多评估 N 样本（0=全量）")
    ap.add_argument("--b2-params", default=None, help="JSON 文件覆盖 B2_PARAMS")
    ap.add_argument("--slumbot-eval", type=int, default=0, help=">0：另评 slumbot 摊牌样本前 N 条")
    args = ap.parse_args()

    params = B2_PARAMS
    if args.b2_params:
        with open(args.b2_params) as f:
            override = json.load(f)
        params = json.loads(json.dumps(B2_PARAMS))
        for k, v in override.items():
            if isinstance(v, dict):
                params[k].update(v)
            else:
                params[k] = v

    print("[m0] 强度表（缓存 strength169.json）…")
    t0 = time.time()
    strength = build_strength_table(samples=args.strength_samples,
                                    cache_path=os.path.join(args.ds, "strength169.json"))
    print(f"[m0] 强度表就绪（{time.time()-t0:.1f}s）")

    with open(os.path.join(args.ds, "live_opp.jsonl")) as f:
        all_samples = [json.loads(l) for l in f]
    splits = {
        "train": set(args.train_runs.split(",")),
        "val": set(args.val_runs.split(",")),
        "test": set(args.test_runs.split(",")),
    }
    by_split = {k: [s for s in all_samples if s["run"] in v] for k, v in splits.items()}
    print(f"[m0] live_opp 样本：train={len(by_split['train'])} val={len(by_split['val'])} "
          f"test={len(by_split['test'])}")

    b1 = B1Population()
    b1.fit(by_split["train"])
    baselines = {
        "B0-uniform": b0_shape,
        "B1-population": b1,
        "B2a-scalar(现状)": B2Scalar(strength),
        "B2b-prior": B2RangeVec(strength, params, use_ops=False),
        "B2b-rangevec": B2RangeVec(strength, params, use_ops=True),
    }
    order = list(baselines.keys())

    report = ["# M0 基线阶梯报告（range 预测）", "",
              f"- 数据：`{args.ds}`；切分 train={sorted(splits['train'])} "
              f"val={sorted(splits['val'])} test={sorted(splits['test'])}",
              f"- 样本数：train={len(by_split['train'])} val={len(by_split['val'])} "
              f"test={len(by_split['test'])}（live_opp 摊牌亮牌样本）",
              f"- 强度表 MC：{args.strength_samples}/类；B2 似然参数：手工默认"
              f"{'（含覆盖）' if args.b2_params else ''}",
              "- 指标口径：4 个街末快照 = 同一批摊牌手 → 分街数即匹配队列；"
              "top-k 平局按（质量, 强度降序）破；排除率 = label 被基线赋零质量的比例。", ""]

    results = {}
    for split in ("val", "test"):
        cache = AxesCache()
        met = eval_samples(by_split[split], baselines, cache, split,
                           limit=args.limit or None)
        results[split] = met
        report += fmt_table(met, order, f"{split} 切分") + [""]

    report.append("### 单调性检验（test，top-5 逐街）")
    report.append("")
    for name in order:
        vals, ok = monotonic_check(results["test"], name)
        report.append(f"- {name}: {' → '.join(f'{v:.3f}' for v in vals)} "
                      f"{'✅ 单调' if ok else '⚠ 非单调'}")
    report.append("")

    # 分对手（test，B2b top-5）
    per_opp = defaultdict(lambda: [0, 0])
    b2b = baselines["B2b-rangevec"]
    cache = AxesCache()
    for s in by_split["test"]:
        board_ids = tuple(card_id(c) for c in s["board"])
        if len(board_ids) < 5:
            continue
        hero_ids = tuple(card_id(c) for c in (s.get("hhole") or []))
        states = HandStates(s)
        cached = {st: cache.get(board_ids[:STREET_NEED[st]],
                                dead_mask_of(hero_ids + board_ids[:STREET_NEED[st]]))
                  for st in (1, 2, 3)}
        ctx = {"sample": s, "street": 3, "states": states, "role": preflop_role(s),
               "prof_life": (s.get("prof") or {}).get("life") or {},
               "axes_fn": lambda st, _c=cached: (_c[st][0], _c[st][1]) if st in _c else None}
        shape = b2b.shape(ctx)
        alive = cached[3][2]
        mass = [sh * al for sh, al in zip(shape, alive)]
        tot = sum(mass) or 1.0
        p = [m / tot for m in mass]
        order5 = sorted(range(169), key=lambda k: (-p[k], k))[:5]
        per_opp[s["tgt"]][0] += 1
        per_opp[s["tgt"]][1] += s["y"] in order5
    report.append("### 分对手（test 河牌末，B2b top-5 命中）")
    report.append("")
    report.append("| 对手 | 样本 | top-5 |")
    report.append("|---|---|---|")
    for nm, (n, hit) in sorted(per_opp.items(), key=lambda kv: -kv[1][0])[:12]:
        report.append(f"| {nm} | {n} | {hit/n:.3f} |")
    report.append("")

    if args.slumbot_eval:
        with open(os.path.join(args.ds, "slumbot.jsonl")) as f:
            slum = [json.loads(l) for l in f if '"fold_end": false' in l][:args.slumbot_eval]
        cache = AxesCache()
        # B1 人群先验是 6-max live 拟合，对 HU 无意义 → 只评 B0/B2 系。
        slim = {k: v for k, v in baselines.items() if not k.startswith("B1")}
        met = eval_samples(slum, slim, cache, "slumbot")
        report += fmt_table(met, [k for k in order if not k.startswith("B1")],
                            f"Slumbot HU 摊牌样本（前 {len(slum)} 条，B1 不适用）") + [""]

    os.makedirs(os.path.dirname(args.report), exist_ok=True)
    with open(args.report, "w") as f:
        f.write("\n".join(report))
    print(f"[m0] 报告 → {args.report}")
    # 控制台摘要：出闸数字
    print("\n=== M0 出闸数字：B2b top-5 命中率（test）===")
    for S in (0, 1, 2, 3):
        m = results["test"].get(("B2b-rangevec", S))
        if m and m["n"]:
            print(f"  {STREET_NAME[S]:8s}: {m['top5']/m['n']:.3f}  (n={m['n']})")


if __name__ == "__main__":
    main()

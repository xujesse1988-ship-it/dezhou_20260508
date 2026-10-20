"""Per-platform analysis: population priors, player profiles, single-spot exploit estimates."""

from __future__ import annotations

import json
import math
from collections import Counter, defaultdict
from dataclasses import dataclass, field
from typing import Optional

from . import exploit
from .cards import river_percentile
from .model import Hand
from .platforms import PLATFORMS
from .spots import hand_observations
from .stats import BetaPrior, Posterior, benjamini_hochberg, fit_prior, posterior, two_sided_p

# (stat, outcome of interest, label) used in the summary table and deviation tests.
KEY_RATES = [
    ("vpip", "yes", "VPIP"),
    ("pfr", "yes", "PFR"),
    ("rfi", "raise", "首入加注"),
    ("rfi", "limp", "首入平跟"),
    ("vs_open", "fold", "面对开池弃"),
    ("vs_open", "raise", "3bet"),
    ("vs_3bet", "fold", "面对3bet弃"),
    ("steal_defense", "fold", "盲位面对偷盲弃"),
    ("cbet", "bet", "翻牌持续下注"),
    ("vs_cbet", "fold", "面对持续下注弃"),
    ("vs_cbet", "raise", "面对持续下注加"),
    ("donk", "bet", "翻牌领先下注"),
    ("barrel_turn", "bet", "转牌二枪"),
    ("barrel_river", "bet", "河牌三枪"),
    ("vs_bet_turn", "fold", "面对转牌下注弃"),
    ("vs_bet_river", "fold", "面对河牌下注弃"),
    ("postflop_aggr", "aggr", "翻后进攻占比(AFq)"),
    ("wtsd", "yes", "WTSD"),
    ("wsd", "won", "W$SD"),
]
SUMMARY = ["vpip", "pfr", "vs_open:raise", "postflop_aggr", "wtsd", "wsd"]


@dataclass
class Config:
    min_hands: int = 100  # players below this get a summary row only
    min_opps: int = 15  # minimum opportunities before a spot-level exploit is listed
    bluff_pct: float = 0.30  # river bettor hand percentile below which a shown hand counts as a bluff
    bluff_pct_alt: tuple = (0.20, 0.40)
    fdr_q: float = 0.10
    drift_z: float = 3.0


@dataclass
class Player:
    name: str
    hands: int = 0
    obs: list = field(default_factory=list)
    order: list = field(default_factory=list)  # (hand index, Obs) for drift checks

    def counts(self, stat: str, where=None) -> Counter:
        c: Counter = Counter()
        for o in self.obs:
            if o.stat == stat and (where is None or where(o)):
                c[o.outcome] += 1
        return c


def _k_n(c: Counter, outcome: str) -> tuple[int, int]:
    return c.get(outcome, 0), sum(c.values())


def reveal_check(hands: list[Hand]) -> dict:
    live = shown = 0
    for h in hands:
        for s in h.showdown_seats:
            live += 1
            shown += s in h.shown
    rate = shown / live if live else float("nan")
    return {"showdown_seats": live, "shown": shown, "rate": rate}


def analyze(hands: list[Hand], platform: str, rejected: list[dict], cfg: Config) -> dict:
    plat = PLATFORMS[platform]
    hero_names = {h.names.get(h.hero_seat) for h in hands} - {None}
    rv = reveal_check(hands)
    clean_showdown = plat.reveal_policy == "all_live" and (rv["showdown_seats"] == 0 or rv["rate"] >= 0.99)
    pct_fn = river_percentile if clean_showdown else None

    players: dict[str, Player] = {}
    forced = Counter()
    for i, h in enumerate(hands):
        for s in h.dealt_seats:
            nm = h.names.get(s)
            if nm:
                players.setdefault(nm, Player(nm)).hands += 1
        for d in h.decisions:
            forced["known" if d.forced is not None else "unknown"] += 1
            if d.forced:
                forced["forced"] += 1
        for o in hand_observations(h, pct_fn):
            p = players[o.name]
            p.obs.append(o)
            p.order.append((i, o))

    opponents = {n: p for n, p in players.items() if n not in hero_names}

    # ---- population priors (opponents only, never other platforms)
    priors: dict[tuple[str, str], BetaPrior] = {}
    for stat, outcome, _ in KEY_RATES:
        priors[(stat, outcome)] = fit_prior([_k_n(p.counts(stat), outcome) for p in opponents.values()])
    clean = lambda o: o.pot_bb is not None  # noqa: E731
    for st in ("flop", "turn", "river"):
        priors[(f"clean_vs_bet_{st}", "fold")] = fit_prior(
            [_k_n(p.counts(f"vs_bet_{st}", clean), "fold") for p in opponents.values()])
    bluff_samples = []
    for p in opponents.values():
        rb = [o for o in p.obs if o.stat == "river_bet_called"]
        bluff_samples.append((sum(o.pct < cfg.bluff_pct for o in rb), len(rb)))
    priors[("river_bluff", "yes")] = fit_prior(bluff_samples)

    # ---- per-player rates + deviation tests vs the rest of the pool
    tests = []
    rows = {}
    for name, p in opponents.items():
        r = {}
        for stat, outcome, label in KEY_RATES:
            k, n = _k_n(p.counts(stat), outcome)
            post = posterior(k, n, priors[(stat, outcome)])
            r[(stat, outcome)] = post
            if n >= 10:
                pool_k = sum(_k_n(q.counts(stat), outcome)[0] for m, q in opponents.items() if m != name)
                pool_n = sum(_k_n(q.counts(stat), outcome)[1] for m, q in opponents.items() if m != name)
                if pool_n:
                    tests.append((name, stat, outcome, label, k, n, pool_k / pool_n, two_sided_p(k, n, pool_k / pool_n)))
        rows[name] = r
    flags = benjamini_hochberg([t[-1] for t in tests], cfg.fdr_q) if tests else []
    deviations = defaultdict(list)
    for t, keep in zip(tests, flags):
        if keep:
            deviations[t[0]].append(t)

    # ---- single-spot exploit estimates
    leaks = defaultdict(list)
    for name, p in opponents.items():
        if p.hands < cfg.min_hands:
            continue
        for st in ("flop", "turn", "river"):
            ob = [o for o in p.obs if o.stat == f"vs_bet_{st}" and o.pot_bb is not None]
            if len(ob) < cfg.min_opps:
                continue
            pots = [o.pot_bb for o in ob]
            bets = [o.bet_bb for o in ob]
            k = sum(o.outcome == "fold" for o in ob)
            post = posterior(k, len(ob), priors[(f"clean_vs_bet_{st}", "fold")])
            rate = 100.0 * len(ob) / p.hands
            g_cons = exploit.overfold_gain(post.q10, pots, bets)
            leaks[name].append({
                "kind": "overfold", "street": st, "n": len(ob), "observed": post,
                "breakeven": exploit.overfold_breakeven(pots, bets),
                "gain_mean": exploit.overfold_gain(post.mean, pots, bets), "gain_cons": g_cons,
                "per100": rate, "index": g_cons * rate,
                "counter": f"{st} 单挑面对首注时多用零胜率牌诈唬",
            })
            if st == "river":
                # continue rate c = 1 - fold; conservative c is the low side
                c_lo, c_mean = 1.0 - post.q90, 1.0 - post.mean
                g_cons = exploit.overcall_gain(c_lo, pots, bets)
                leaks[name].append({
                    "kind": "overcall", "street": st, "n": len(ob), "observed": post,
                    "breakeven": exploit.overcall_breakeven(pots, bets),
                    "gain_mean": exploit.overcall_gain(c_mean, pots, bets), "gain_cons": g_cons,
                    "per100": rate, "index": g_cons * rate,
                    "counter": "河牌单挑减少纯诈唬（他跟得太多）",
                })
        rb = [o for o in p.obs if o.stat == "river_bet_called"]
        if pct_fn is not None and len(rb) >= cfg.min_opps:
            pots = [o.pot_bb for o in rb]
            bets = [o.bet_bb for o in rb]
            k = sum(o.pct < cfg.bluff_pct for o in rb)
            post = posterior(k, len(rb), priors[("river_bluff", "yes")])
            alt = {thr: sum(o.pct < thr for o in rb) / len(rb) for thr in cfg.bluff_pct_alt}
            rate = 100.0 * sum(1 for o in p.obs if o.stat == "bet_first_river" and o.outcome == "bet") / p.hands
            be = exploit.bluffcatch_breakeven(pots, bets)
            fold_cons = -exploit.bluffcatch_ev(post.q90, pots, bets)
            call_cons = exploit.bluffcatch_ev(post.q10, pots, bets)
            base = {"street": "river", "n": len(rb), "observed": post, "breakeven": be,
                    "per100": rate, "alt_raw": alt}
            leaks[name].append(dict(base, kind="underbluff", gain_mean=-exploit.bluffcatch_ev(post.mean, pots, bets),
                                    gain_cons=fold_cons, index=fold_cons * rate,
                                    counter="他河牌下注时，只能抓诈唬的牌弃掉"))
            leaks[name].append(dict(base, kind="overbluff", gain_mean=exploit.bluffcatch_ev(post.mean, pots, bets),
                                    gain_cons=call_cons, index=call_cons * rate,
                                    counter="他河牌下注时，用更多抓诈唬的牌跟注"))
        # A row whose mean gain is not positive is not a leak at all; keep only candidates.
        leaks[name] = sorted((x for x in leaks[name] if x["gain_mean"] > 0), key=lambda x: x["index"], reverse=True)

    # ---- drift: first half vs second half of each opponent's hands
    drift = defaultdict(list)
    for name, p in opponents.items():
        if p.hands < cfg.min_hands:
            continue
        idx = sorted({i for i, _ in p.order})
        cut = idx[len(idx) // 2]
        for stat, outcome in (("vpip", "yes"), ("pfr", "yes"), ("postflop_aggr", "aggr")):
            a = [o.outcome == outcome for i, o in p.order if o.stat == stat and i < cut]
            b = [o.outcome == outcome for i, o in p.order if o.stat == stat and i >= cut]
            if len(a) < 30 or len(b) < 30:
                continue
            pa, pb = sum(a) / len(a), sum(b) / len(b)
            pp = (sum(a) + sum(b)) / (len(a) + len(b))
            se = math.sqrt(pp * (1 - pp) * (1 / len(a) + 1 / len(b))) if 0 < pp < 1 else 0
            z = (pb - pa) / se if se else 0.0
            if abs(z) >= cfg.drift_z:
                # Table-size mix per half: a shift there can fake a drift.
                small = [o.dims.get("table") == "2-4" for i, o in p.order if o.stat == stat]
                sa = sum(small[: len(a)]) / len(a)
                sb = sum(small[len(a):]) / len(b)
                drift[name].append((stat, pa, pb, z, len(a), len(b), sa, sb))

    # ---- table-size split for VPIP (mixing table sizes can mislead)
    by_table = {}
    for name, p in opponents.items():
        if p.hands >= cfg.min_hands:
            by_table[name] = {tb: _k_n(p.counts("vpip", lambda o, tb=tb: o.dims.get("table") == tb), "yes")
                              for tb in ("2-4", "5-6", "7-9")}

    return {
        "platform": platform, "plat": plat, "hands": len(hands), "rejected": Counter(r["code"] for r in rejected),
        "hero_names": sorted(hero_names), "reveal": rv, "clean_showdown": clean_showdown, "forced": forced,
        "priors": priors, "players": players, "opponents": opponents, "rows": rows,
        "deviations": deviations, "n_tests": len(tests), "leaks": leaks, "drift": drift, "by_table": by_table,
        "cfg": cfg,
    }


# ------------------------------------------------------------------ rendering

def _pct(x: Optional[float]) -> str:
    return "—" if x is None or (isinstance(x, float) and math.isnan(x)) else f"{100 * x:.1f}"


def _ci(p: Posterior) -> str:
    if p.n == 0:
        return "—"
    return f"{_pct(p.mean)} [{_pct(p.lo)}, {_pct(p.hi)}] n={p.n}"


def _tag(r: dict) -> str:
    v = r[("vpip", "yes")]
    f = r[("pfr", "yes")]
    a = r[("postflop_aggr", "aggr")]
    if v.n < 50:
        return "样本不足"
    loose = "松" if v.mean >= 0.30 else ("紧" if v.mean <= 0.15 else "中")
    ratio = f.mean / v.mean if v.mean > 0 else 0
    pre = "凶" if ratio >= 0.6 else ("被动" if ratio <= 0.35 else "中")
    post = "" if a.n < 30 else ("，翻后被动" if a.mean <= 0.25 else ("，翻后凶" if a.mean >= 0.5 else ""))
    return f"{loose}/{pre}{post}"


LEAK_NAMES = {"overfold": "弃牌过多", "overcall": "跟注过多", "underbluff": "河牌诈唬过少", "overbluff": "河牌诈唬过多"}


def render_markdown(res: dict) -> str:
    cfg: Config = res["cfg"]
    plat = res["plat"]
    rv = res["reveal"]
    L = []
    L.append(f"# 对手可剥削性报告 — {res['platform']}")
    L.append("")
    L.append("只含本平台数据，群体基准也只由本平台对手算出。百分数为后验均值 [5%, 95%]，n 为机会数。")
    L.append("")
    L.append("## 1. 数据")
    L.append("")
    L.append(f"- 通过校验的手数：{res['hands']}；被拒：{sum(res['rejected'].values())}"
             + ("（" + "，".join(f"{k} {v}" for k, v in res["rejected"].most_common()) + "）" if res["rejected"] else ""))
    L.append(f"- 我方名字（已排除在对手统计之外）：{', '.join(res['hero_names']) or '—'}")
    fz = res["forced"]
    L.append(f"- 服务器代打（超时）动作：已识别 {fz.get('forced', 0)} 次已剔除；无法判断是否代打的决策 {fz.get('unknown', 0)} 次"
             + ("（缺 ws 日志，超时弃牌会混在弃牌里）" if fz.get("unknown") else ""))
    L.append(f"- 摊牌亮牌：平台规则 `{plat.reveal_policy}`；实测摊牌时仍在牌里的 {rv['showdown_seats']} 个座位中亮出 {rv['shown']} 个"
             f"（{_pct(rv['rate'])}%）。河牌诈唬比例分析：{'启用' if res['clean_showdown'] else '停用（规则或实测不支持无偏样本）'}")
    L.append("")
    L.append("## 2. 群体基准")
    L.append("")
    L.append("| 统计 | 群体均值 | 先验强度 k |")
    L.append("|---|---|---|")
    for stat, outcome, label in KEY_RATES:
        pr = res["priors"][(stat, outcome)]
        L.append(f"| {label} | {_pct(pr.mean)} | {pr.strength:.0f} |")
    L.append("")
    L.append("k 越大，说明对手之间在这项上差别越小，样本少的玩家会被更强地拉向群体均值。")
    L.append("")
    L.append("## 3. 对手总表")
    L.append("")
    L.append("| 玩家 | 手数 | 类型 | VPIP | PFR | 3bet | 翻后进攻占比 | WTSD | W$SD |")
    L.append("|---|---|---|---|---|---|---|---|---|")
    opp = sorted(res["opponents"].values(), key=lambda p: -p.hands)
    for p in opp:
        r = res["rows"][p.name]
        L.append(f"| {p.name} | {p.hands} | {_tag(r)} | {_ci(r[('vpip','yes')])} | {_ci(r[('pfr','yes')])} | "
                 f"{_ci(r[('vs_open','raise')])} | {_ci(r[('postflop_aggr','aggr')])} | {_ci(r[('wtsd','yes')])} | {_ci(r[('wsd','won')])} |")
    L.append("")
    L.append(f"## 4. 逐个对手（手数 ≥ {cfg.min_hands}）")
    for p in opp:
        if p.hands < cfg.min_hands:
            continue
        r = res["rows"][p.name]
        L.append("")
        L.append(f"### {p.name}（{p.hands} 手，{_tag(r)}）")
        L.append("")
        lk = res["leaks"].get(p.name, [])
        if lk:
            L.append("单点剥削估计（bb；保守 = 取对我方不利一侧的 10% 分位；指数 = 保守收益 × 每 100 手出现次数）：")
            L.append("")
            L.append("| 漏洞 | 观测 | 保本点 | 每次收益 均值 / 保守 | 每100手出现 | 指数 | 应对 |")
            L.append("|---|---|---|---|---|---|---|")
            for x in lk:
                mark = "" if x["gain_cons"] > 0 else "（保守值不为正，不能确认）"
                obs = _ci(x["observed"])
                if x["kind"] in ("underbluff", "overbluff"):
                    obs = "诈唬比例 " + obs + "；阈值 " + "/".join(f"{int(100*t)}%→{_pct(v)}" for t, v in x["alt_raw"].items())
                elif x["kind"] == "overcall":
                    obs = "弃牌 " + obs + "（继续 = 1 − 弃牌）"
                else:
                    obs = "弃牌 " + obs
                L.append(f"| {LEAK_NAMES[x['kind']]}·{x['street']}{mark} | {obs} | {_pct(x['breakeven'])} | "
                         f"{x['gain_mean']:+.2f} / {x['gain_cons']:+.2f} | {x['per100']:.1f} | {x['index']:+.2f} | {x['counter']} |")
            L.append("")
        else:
            L.append(f"单点剥削估计：没有均值为正的候选（或可用机会不足 {cfg.min_opps} 次）。")
            L.append("")
        dv = res["deviations"].get(p.name, [])
        if dv:
            L.append(f"与群体显著不同的统计（{res['n_tests']} 项检验，错误发现率控制在 {int(cfg.fdr_q*100)}%）：")
            L.append("")
            for (_, stat, outcome, label, k, n, pool, pv) in sorted(dv, key=lambda t: t[-1]):
                L.append(f"- {label}：{_pct(k / n)}%（{k}/{n}），其他对手 {_pct(pool)}%，p={pv:.1e}")
            L.append("")
        dr = res["drift"].get(p.name, [])
        for stat, pa, pb, z, na, nb, sa, sb in dr:
            L.append(f"- ⚠ 漂移：{stat} 前半 {_pct(pa)}%（{na}）→ 后半 {_pct(pb)}%（{nb}），z={z:+.1f}；"
                     f"2–4 人桌占比 前半 {_pct(sa)}% / 后半 {_pct(sb)}%（占比变化大时先怀疑桌况，不是换打法）")
        bt = res["by_table"].get(p.name)
        if bt and sum(1 for k, n in bt.values() if n >= 30) >= 2:
            L.append("- VPIP 按桌上人数：" + "，".join(f"{tb} 人 {_pct(k/n)}%（{n}）" for tb, (k, n) in bt.items() if n))
        L.append("")
        L.append("<details><summary>全部统计</summary>")
        L.append("")
        L.append("| 统计 | 后验 |")
        L.append("|---|---|")
        for stat, outcome, label in KEY_RATES:
            L.append(f"| {label} | {_ci(r[(stat, outcome)])} |")
        L.append("")
        L.append("</details>")
    L.append("")
    L.append("## 5. 读法")
    L.append("")
    L.append("- 每次收益按「我方在同样的局面、同样的下注尺寸做出这个改动」计算：弃牌过多 = 用零胜率牌下注，"
             "被加注就弃；跟注过多 = 少打一次河牌纯诈唬；河牌诈唬过少 / 过多 = 只能抓诈唬的牌面对他河牌下注时弃 / 跟。")
    L.append("- 河牌公式是精确的；翻牌、转牌的诈唬公式没算诈唬牌自带的胜率和后续街，是下限。")
    L.append("- 「每100手出现」数的是这个对手遇到该局面的次数（不管下注的是谁），所以指数是「如果每次都由我方来利用」的上限，"
             "只用来排序，不是预期赢率。")
    L.append(f"- 河牌诈唬比例只用「河牌单挑首注被跟注后直接摊牌」的手（这类手是他河牌下注范围的近似无偏样本）；"
             f"诈唬 = 他的牌在该牌面所有可能底牌中排名低于 {int(cfg.bluff_pct*100)}%。表里同时给了阈值取 "
             + "/".join(f"{int(t*100)}%" for t in cfg.bluff_pct_alt) + " 时的原始比例，结论随阈值翻转的不要用。")
    return "\n".join(L) + "\n"


def profile_json(res: dict) -> dict:
    def post(p: Posterior) -> dict:
        return {"k": p.k, "n": p.n, "mean": p.mean, "lo5": p.lo, "hi95": p.hi}

    out = {"platform": res["platform"], "hands": res["hands"], "schema": 1, "players": {}}
    for name, p in res["opponents"].items():
        r = res["rows"][name]
        out["players"][name] = {
            "hands": p.hands,
            "rates": {f"{s}:{o}": post(r[(s, o)]) for s, o, _ in KEY_RATES},
            "leaks": [{k: (post(v) if isinstance(v, Posterior) else v) for k, v in x.items()}
                      for x in res["leaks"].get(name, [])],
        }
    out["priors"] = {f"{s}:{o}": {"mean": pr.mean, "k": pr.strength} for (s, o), pr in res["priors"].items()}
    return out


def write_report(res: dict, out_dir: str) -> tuple[str, str]:
    import os

    os.makedirs(out_dir, exist_ok=True)
    md = os.path.join(out_dir, f"report_{res['platform']}.md")
    js = os.path.join(out_dir, f"profile_{res['platform']}.json")
    with open(md, "w") as f:
        f.write(render_markdown(res))
    with open(js, "w") as f:
        json.dump(profile_json(res), f, ensure_ascii=False, indent=1)
    return md, js

#!/usr/bin/env python3
"""M1 基线 B3：LightGBM on 每街聚合特征 → 169 类（docs/opponent_range_ml_design.md §3b/§7）。

每街一套 multiclass 模型（B3 的「anytime」粒度就是每街一次——这正是它相对序列模型
的结构性短板，作为反证基线保留）。训练混源带权（live_opp 1.0 / hero 0.3 / slumbot
0.1，与序列模型同口径）；早停看 run28 val（live_opp）multi_logloss。

评估与 M0 harness 同口径：mass = P(class)/combo_weight × alive，top-k 平局按
(-p, k) 破；报告分街 top-1/5/10、NLL、单调检验，并与 B2b（prior 快照）配对比较。

用法：
  .venv-rangeml/bin/python tools/range_b3_lgbm.py                 # 训练 + 评估
  .venv-rangeml/bin/python tools/range_b3_lgbm.py --live-opp-only # 消融：不混辅助源
"""

import argparse
import json
import math
import os
import sys
import time

import lightgbm as lgb
import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from range_ml_common import combo_weight  # noqa: E402

STREET_NAME = {0: "preflop", 1: "flop", 2: "turn", 3: "river"}
COMBO_W = np.array([combo_weight(k) for k in range(169)], dtype=np.float64)


def eval_probs(p_class, alive, y):
    """单行：类概率 + alive → (rank, p_label)。与 M0 harness 同口径。"""
    mass = (p_class / COMBO_W) * alive
    tot = mass.sum()
    if tot <= 0:
        mass = alive.astype(np.float64)
        tot = mass.sum()
    p = mass / tot
    order = np.lexsort((np.arange(169), -p))
    rank = int(np.nonzero(order == y)[0][0])
    return rank, float(p[y])


def summarize(rows):
    """rows: [(rank, p_label)] → 指标 dict。"""
    n = len(rows)
    if not n:
        return None
    ranks = np.array([r for r, _ in rows])
    ps = np.array([p for _, p in rows])
    return {
        "n": n,
        "top1": float((ranks < 1).mean()), "top5": float((ranks < 5).mean()),
        "top10": float((ranks < 10).mean()),
        "nll": float(-np.log(np.maximum(ps, 1e-12)).mean()),
        "rank_mean": float(ranks.mean() + 1), "rank_med": int(np.median(ranks) + 1),
        "mass": float(ps.mean()),
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--ds", default="artifacts/range_ml")
    ap.add_argument("--report", default="artifacts/range_ml/m1_b3_report.md")
    ap.add_argument("--live-opp-only", action="store_true")
    ap.add_argument("--rounds", type=int, default=400)
    ap.add_argument("--lr", type=float, default=0.08)
    ap.add_argument("--leaves", type=int, default=31)
    ap.add_argument("--min-data", type=int, default=20)
    ap.add_argument("--ff", type=float, default=0.9, help="feature_fraction")
    args = ap.parse_args()

    d = np.load(os.path.join(args.ds, "b3_v1.npz"))
    x, street, y, w = d["x"], d["street"], d["y"].astype(np.int32), d["w"]
    src, split, alive = d["src"], d["split"], d["alive"]
    if args.live_opp_only:
        keep = src == 0
    else:
        keep = np.ones(len(x), dtype=bool)

    report = ["# M1 B3（LightGBM 每街聚合特征）", "",
              f"- 特征维度 {x.shape[1]}；训练混源：{'仅 live_opp' if args.live_opp_only else 'live_opp 1.0 / hero 0.3 / slumbot 0.1'}",
              f"- 参数：leaves={args.leaves} lr={args.lr} rounds≤{args.rounds}（早停 30 看 val logloss）", ""]
    results = {}
    for S in range(4):
        m_tr = keep & (street == S) & (split == 0)
        m_va = (src == 0) & (street == S) & (split == 1)
        m_te = (src == 0) & (street == S) & (split == 2)
        t0 = time.time()
        train_set = lgb.Dataset(x[m_tr], label=y[m_tr], weight=w[m_tr])
        val_set = lgb.Dataset(x[m_va], label=y[m_va], reference=train_set)
        booster = lgb.train(
            {"objective": "multiclass", "num_class": 169, "learning_rate": args.lr,
             "num_leaves": args.leaves, "min_data_in_leaf": args.min_data,
             "feature_fraction": args.ff,
             "bagging_fraction": 0.8, "bagging_freq": 1, "num_threads": 2,
             "verbosity": -1, "metric": "multi_logloss"},
            train_set, num_boost_round=args.rounds, valid_sets=[val_set],
            callbacks=[lgb.early_stopping(30, verbose=False)],
        )
        for split_id, name in ((1, "val"), (2, "test")):
            m_ev = (src == 0) & (street == S) & (split == split_id)
            probs = booster.predict(x[m_ev], num_iteration=booster.best_iteration)
            rows = [eval_probs(probs[i], alive[np.nonzero(m_ev)[0][i]], y[m_ev][i])
                    for i in range(len(probs))]
            results[(name, S)] = summarize(rows)
        print(f"[b3] street {STREET_NAME[S]}: best_iter={booster.best_iteration} "
              f"({time.time()-t0:.0f}s) test top5={results[('test', S)]['top5']:.3f}", flush=True)

    for name in ("val", "test"):
        report += [f"### {name} 切分（live_opp）", "",
                   "| 街 | top-1 | **top-5** | top-10 | NLL | rank 均值 | rank 中位 | label 质量 |",
                   "|---|---|---|---|---|---|---|---|"]
        for S in range(4):
            m = results.get((name, S))
            if m:
                report.append(f"| {STREET_NAME[S]} | {m['top1']:.3f} | **{m['top5']:.3f}** | "
                              f"{m['top10']:.3f} | {m['nll']:.3f} | {m['rank_mean']:.1f} | "
                              f"{m['rank_med']} | {m['mass']:.4f} |")
        report.append("")
    vals = [results[("test", S)]["top5"] for S in range(4) if results.get(("test", S))]
    mono = all(vals[i] <= vals[i + 1] + 1e-9 for i in range(len(vals) - 1))
    report.append(f"单调性（test top-5）：{' → '.join(f'{v:.3f}' for v in vals)} "
                  f"{'✅ 单调' if mono else '⚠ 非单调'}")
    report.append("")
    with open(args.report, "w") as f:
        f.write("\n".join(report))
    print(f"[b3] 报告 → {args.report}")
    print(json.dumps({f"{k[0]}_{STREET_NAME[k[1]]}": round(v["top5"], 3)
                      for k, v in results.items()}, ensure_ascii=False))


if __name__ == "__main__":
    main()

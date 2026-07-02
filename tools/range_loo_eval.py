#!/usr/bin/env python3
"""M2 LOO（leave-opponents-out）meta 泛化评估（docs/opponent_range_ml_design.md §7/§9）。

问题：没见过的对手，画像通道还能不能用？出闸（§9 M2）：未见对手 ≥ B2b。

折叠式 LOO：亮牌样本 ≥ --min-samples 的对手按样本数贪心均衡分 K 折；每折重训
（train 切分剔除：该折对手的全部样本 + **同 hid 事件流的一切样本**——多路底池一手
出多个样本、hero 自标注同手共享事件流，防背题式泄漏），在该折对手的**全部** live_opp
样本上评估（模型从未见过该对手 → 无泄漏，全量取功效），vs B2b（残差=0）配对检验。

早停：常规 val 切分（run28 live_opp）同样剔除该折对手/同手样本。

用法：
  OMP_NUM_THREADS=2 .venv-rangeml/bin/python tools/range_loo_eval.py   # ~30min
"""

import argparse
import json
import os
import sys
from collections import Counter, defaultdict

import numpy as np
import torch

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from range_censoring_ab import train_model  # noqa: E402
from range_train import (  # noqa: E402
    STREET_NAME, SeqData, paired_bootstrap, snapshot_eval,
)


def load_hids(ds, data, train_runs, val_runs, test_runs):
    """live_opp + live_hero 的 hid（featurize 同款过滤保序），slumbot 填独占位符。"""
    splits = {"train": set(train_runs.split(",")), "val": set(val_runs.split(",")),
              "test": set(test_runs.split(","))}
    hids = np.empty(data.n, dtype=object)
    pos = {0: np.nonzero(data.src == 0)[0], 1: np.nonzero(data.src == 1)[0]}
    for src_id, fname in ((0, "live_opp.jsonl"), (1, "live_hero.jsonl")):
        j = 0
        with open(os.path.join(ds, fname)) as f:
            for line in f:
                s = json.loads(line)
                run = s.get("run", "")
                if src_id == 0:
                    if not any(run in v for v in splits.values()):
                        continue
                elif run not in splits["train"]:
                    continue
                gi = pos[src_id][j]
                assert data.y[gi] == s["y"], f"{fname} 对拍失败 @{j}"
                hids[gi] = s["hid"]
                j += 1
        assert j == len(pos[src_id]), f"{fname} 行数不齐：{j} != {len(pos[src_id])}"
    for gi in np.nonzero(data.src == 2)[0]:
        hids[gi] = f"__slum{gi}"
    return hids


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--ds", default="artifacts/range_ml")
    ap.add_argument("--report", default="artifacts/range_ml/m2_loo_report.md")
    ap.add_argument("--folds", type=int, default=6)
    ap.add_argument("--min-samples", type=int, default=50)
    ap.add_argument("--train-runs", default="run24,run25,run26,run27")
    ap.add_argument("--val-runs", default="run28")
    ap.add_argument("--test-runs", default="run29,run30")
    ap.add_argument("--d", type=int, default=192)
    ap.add_argument("--dropout", type=float, default=0.15)
    ap.add_argument("--epochs", type=int, default=120)
    ap.add_argument("--batch", type=int, default=96)
    ap.add_argument("--lr", type=float, default=5e-4)
    ap.add_argument("--wd", type=float, default=1e-4)
    ap.add_argument("--eps", type=float, default=0.05)
    ap.add_argument("--patience", type=int, default=12)
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--max-folds", type=int, default=0, help="调试：只跑前 N 折")
    args = ap.parse_args()

    torch.set_num_threads(2)
    data = SeqData(os.path.join(args.ds, "seq_v1.npz"))
    hids = load_hids(args.ds, data, args.train_runs, args.val_runs, args.test_runs)
    tgt = np.load(os.path.join(args.ds, "seq_v1.npz"))["tgt"]

    live = np.nonzero(data.src == 0)[0]
    cnt = Counter(tgt[live])
    eligible = [(nm, n) for nm, n in cnt.most_common() if n >= args.min_samples]
    folds = [[] for _ in range(args.folds)]
    fold_n = [0] * args.folds
    for nm, n in eligible:  # 贪心均衡（按样本数降序放最轻折）
        k = int(np.argmin(fold_n))
        folds[k].append(nm)
        fold_n[k] += n
    print(f"[loo] 对手 ≥{args.min_samples} 样本：{len(eligible)} 个 → {args.folds} 折 "
          f"{fold_n}", flush=True)

    all_mats, all_tgt = [], []
    for k, fold in enumerate(folds):
        if args.max_folds and k >= args.max_folds:
            break
        fold_set = set(fold)
        eval_mask = (data.src == 0) & np.isin(tgt, fold)
        eval_idx = np.nonzero(eval_mask)[0]
        excl_hids = set(hids[eval_idx])
        hid_excl = np.array([h in excl_hids for h in hids])
        train_idx = np.nonzero((data.split == 0) & ~hid_excl & ~np.isin(tgt, fold))[0]
        val_idx = np.nonzero((data.split == 1) & (data.src == 0) & ~hid_excl
                             & ~np.isin(tgt, fold))[0]
        n_cut = int((data.split == 0).sum() - len(train_idx))
        print(f"[loo] fold{k} [{', '.join(fold[:4])}{'…' if len(fold) > 4 else ''}] "
              f"eval={len(eval_idx)} train剔除={n_cut}", flush=True)
        model = train_model(data, train_idx, val_idx, args, f"fold{k}")
        _, mats = snapshot_eval(model, data, eval_idx, with_prior_ref=True)
        all_mats.append(mats)
        all_tgt.append(tgt[eval_idx])

    hit5 = np.concatenate([m[0] for m in all_mats])
    nll = np.concatenate([m[1] for m in all_mats])
    ref_hit5 = np.concatenate([m[2] for m in all_mats])
    ref_nll = np.concatenate([m[3] for m in all_mats])
    tgts = np.concatenate(all_tgt)

    report = ["# M2 LOO meta 泛化报告（leave-opponents-out）", "",
              f"- {len(eligible)} 个对手（≥{args.min_samples} 亮牌样本）分 {args.folds} 折，"
              "每折重训剔除该折对手全部样本 + 同 hid 事件流样本；在其全部样本上评估",
              f"- 覆盖样本：{len(tgts)}；对照 = B2b（残差=0，画像仅标量平移——不学习）",
              "- 出闸口径：未见对手 pooled ≥ B2b", "",
              "### pooled（全部 held-out 样本，配对）", "",
              "| 街 | n | LOO top-5 | B2b top-5 | Δtop-5 [CI] p | LOO NLL | B2b NLL | ΔNLL p |",
              "|---|---|---|---|---|---|---|---|"]
    for S in range(4):
        m = hit5[:, S] >= 0
        d5, (lo, hi), p5 = paired_bootstrap(hit5[m, S], ref_hit5[m, S])
        dn, _, pn = paired_bootstrap(-nll[m, S], -ref_nll[m, S])
        report.append(f"| {STREET_NAME[S]} | {int(m.sum())} | {hit5[m, S].mean():.3f} | "
                      f"{(ref_hit5[m, S] == 1).mean():.3f} | {d5:+.3f} [{lo:+.3f}, {hi:+.3f}] "
                      f"{p5:.4f} | {nll[m, S].mean():.3f} | {ref_nll[m, S].mean():.3f} | "
                      f"{-dn:+.3f} {pn:.4f} |")

    per_opp = defaultdict(lambda: [0, 0, 0, 0.0, 0.0])  # n_pos, hit, ref_hit, nll, ref_nll
    for i in range(len(tgts)):
        for S in range(4):
            if hit5[i, S] < 0:
                continue
            a = per_opp[tgts[i]]
            a[0] += 1
            a[1] += int(hit5[i, S] == 1)
            a[2] += int(ref_hit5[i, S] == 1)
            a[3] += nll[i, S]
            a[4] += ref_nll[i, S]
    report += ["", "### 分对手（4 街 pooled top-5）", "",
               "| 对手 | 快照数 | LOO | B2b | Δ | ΔNLL |", "|---|---|---|---|---|---|"]
    n_win = 0
    for nm, a in sorted(per_opp.items(), key=lambda kv: -kv[1][0]):
        d = a[1] / a[0] - a[2] / a[0]
        n_win += d >= 0
        report.append(f"| {nm} | {a[0]} | {a[1]/a[0]:.3f} | {a[2]/a[0]:.3f} | {d:+.3f} | "
                      f"{(a[3]-a[4])/a[0]:+.3f} |")
    report += ["", f"对手层面：{n_win}/{len(per_opp)} 个 LOO ≥ B2b（top-5）"]

    with open(args.report, "w") as f:
        f.write("\n".join(report))
    print(f"[loo] 报告 → {args.report}", flush=True)
    for line in report[6:16]:
        print(line)
    print(report[-1])


if __name__ == "__main__":
    main()

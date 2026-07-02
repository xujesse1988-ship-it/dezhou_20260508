#!/usr/bin/env python3
"""M2 翻后校准：per-street 残差温度扫描（M1 遗留课题）。

街 S 的快照指标只依赖 s_S ⇒ 标量网格一次扫完四街：在 val 上对 s ∈ grid 各评一次，
每街独立选 |q_mean−50%| 最小的 s_S（q 是排序校准的 PIT 读数，均值 50% = 无系统性
过强/过宽），拼成逐街向量后在 test 上复评（含 equity 残差），与最优标量对照。

用法：
  OMP_NUM_THREADS=2 .venv-rangeml/bin/python tools/range_rscale_scan.py \
      --ckpt artifacts/range_ml/range_v1_gru.pt
"""

import argparse
import os
import sys

import numpy as np
import torch

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from range_train import (  # noqa: E402
    STREET_NAME, ResidualGRU, SeqData, paired_bootstrap, snapshot_eval,
)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--ds", default="artifacts/range_ml")
    ap.add_argument("--ckpt", default="artifacts/range_ml/range_v1_gru.pt")
    ap.add_argument("--shows-npz", default="artifacts/range_ml/shows_v1.npz")
    ap.add_argument("--eqtab", default="artifacts/range_ml/eqtab_v1.npz")
    ap.add_argument("--grid", default="0.5,0.6,0.7,0.8,0.9,1.0")
    args = ap.parse_args()

    torch.set_num_threads(2)
    ck = torch.load(args.ckpt, map_location="cpu", weights_only=False)
    use_shows = ck.get("use_shows", False)
    data = SeqData(os.path.join(args.ds, "seq_v1.npz"),
                   shows_path=args.shows_npz if use_shows else None)
    model = ResidualGRU(ck["fdim"], d=ck["d"], dropout=0.0, use_shows=use_shows)
    model.load_state_dict(ck["model"])
    print(f"[scan] {args.ckpt} (ep{ck['epoch']}, use_shows={use_shows})", flush=True)

    eqtab = None
    if os.path.exists(args.eqtab):
        et = np.load(args.eqtab)
        eqtab = ({int(si): j for j, si in enumerate(et["sample"])},
                 et["eqc"].astype(np.float32), et["eqy"], et["valid"])

    val_idx = np.nonzero((data.split == 1) & (data.src == 0))[0]
    test_idx = np.nonzero((data.split == 2) & (data.src == 0))[0]
    grid = [float(x) for x in args.grid.split(",")]

    val_q = {}
    print("\nval：q 均值（|q−50%| 最小者为该街选点）；括号内 top-5")
    print("| s | " + " | ".join(STREET_NAME[S] for S in range(4)) + " |")
    for s in grid:
        met, _ = snapshot_eval(model, data, val_idx, rscale=s)
        val_q[s] = {S: met[S]["q_mean"] for S in met}
        print(f"| {s:.1f} | " + " | ".join(
            f"{met[S]['q_mean']*100:.1f}% ({met[S]['top5']:.3f})" for S in range(4)) + " |",
            flush=True)

    chosen = [min(grid, key=lambda s: abs(val_q[s][S] - 0.5)) for S in range(4)]
    print(f"\n[scan] 逐街选点（val |q_mean−50%| 最小）：{chosen}")

    for tag, rs in (("标量 0.8", 0.8), (f"逐街 {chosen}", chosen)):
        met, mats = snapshot_eval(model, data, test_idx, with_prior_ref=True,
                                  rscale=rs, eqtab=eqtab)
        hit5, _, ref_hit5 = mats[0], mats[1], mats[2]
        print(f"\ntest @ {tag}：")
        for S in range(4):
            m = hit5[:, S] >= 0
            d5, _, p5 = paired_bootstrap(hit5[m, S], ref_hit5[m, S])
            eq = (f"eq_bias {met[S]['eq_bias']*100:+.1f}pp eq_MAE {met[S]['eq_mae']*100:.1f}"
                  if "eq_bias" in met[S] else "")
            print(f"  {STREET_NAME[S]:8s}: top5 {met[S]['top5']:.3f} "
                  f"(Δ{d5:+.3f} p={p5:.4f})  q_mean {met[S]['q_mean']*100:.1f}%  "
                  f"NLL {met[S]['nll']:.3f}  {eq}", flush=True)


if __name__ == "__main__":
    main()

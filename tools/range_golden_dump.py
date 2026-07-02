#!/usr/bin/env python3
"""M3 §6 golden 对拍 oracle：导出 Rust `range.rs` 手写 range 需复现的因果 m_prior。

对 N 个（跨 run 均匀采样的）live_opp 样本，复用 `SampleFeaturizer`（B2b 因果先验的
**权威 Python 参考**）算出每个街末快照的：
  - `s`         该样本的完整事件流（Rust 端输入：sb/bb/tseat/ev/board/hhole/prof.life…）
  - `logprior`  该街末 token 位的因果 log m_prior [169]（= log(base_prior · Π op)，f32）
  - `alive`     该街末观察者视角存活 combo 数 [169]
  - `p`         B2b 预测 = softmax(logprior + log alive，死类 −∞)[169]（Rust 端对拍目标）
  - `snap`/`y`  快照 token 下标 / 亮牌 169 类

Rust 侧读本文件，用移植的 prior+op 因果链重算 logprior，比对：softmax-p max|Δ|<1e-4
且逐街 top-5 一致（§6「双端逐位比对」的可执行形式）。alive 用本文件的值（其
canonical_index/combo 枚举已证与 Rust 逐义一致，不重复测），故本对拍纯测新代码：
strength169 载入 + threshold_for_top + 模板 prior + 逐动作 op + 因果角色重算。

用法：
  .venv-rangeml/bin/python tools/range_golden_dump.py            # 200 样本 → 默认路径
  .venv-rangeml/bin/python tools/range_golden_dump.py --n 300 --out <path>
"""

import argparse
import json
import math
import os
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from range_ml_common import build_strength_table  # noqa: E402
from range_baselines import B2_PARAMS, B2RangeVec  # noqa: E402
from range_featurize import SampleFeaturizer  # noqa: E402
from range_shadow import NpResidualGRU  # noqa: E402


def b2b_p(logprior, alive):
    """B2b 预测：softmax(logprior + log alive)，alive==0 掩 −inf。与 featurize.parity_check
    / range_train.masked_logits(res=0) 同口径。"""
    av = np.asarray(alive, dtype=np.float64)
    lp = np.asarray(logprior, dtype=np.float64) + np.log(np.maximum(av, 1e-9))
    lp[av == 0] = -1e30
    z = lp - lp.max()
    p = np.exp(z)
    p /= p.sum()
    return p


def pred_p(logprior, res_sp, alive, rscale):
    """ML 预测：softmax(logprior + rscale·residual + log max(alive,1))，死类掩 −inf。
    逐义复刻 range_train.masked_logits + snapshot_eval（Rust mlrange 前向对拍目标）。"""
    av = np.asarray(alive, dtype=np.float64)
    la = np.log(np.maximum(av, 1.0))
    lp = np.asarray(logprior, dtype=np.float64) + rscale * np.asarray(res_sp, dtype=np.float64) + la
    lp[av == 0] = -1e30
    z = lp - lp.max()
    p = np.exp(z)
    p /= p.sum()
    return p


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--ds", default="artifacts/range_ml")
    ap.add_argument("--opp", default="artifacts/range_ml/live_opp.jsonl")
    ap.add_argument("--out", default="artifacts/range_ml/golden_mprior.json")
    ap.add_argument("--n", type=int, default=200, help="导出样本数（跨全集均匀采样）")
    ap.add_argument("--ckpt", default="artifacts/range_ml/range_v1_gru.pt")
    ap.add_argument("--rscale", type=float, default=0.8, help="ML 预测残差温度（生产配置）")
    args = ap.parse_args()

    strength = build_strength_table(cache_path=os.path.join(args.ds, "strength169.json"))
    fz = SampleFeaturizer(B2RangeVec(strength, B2_PARAMS, use_ops=True), {})
    model = NpResidualGRU(args.ckpt) if os.path.exists(args.ckpt) else None
    if model is None:
        print(f"[golden] ckpt {args.ckpt} 缺席 → 只出 m_prior（无 feat/pred）", flush=True)

    with open(args.opp) as f:
        all_samples = [json.loads(line) for line in f]
    stride = max(1, len(all_samples) // args.n)
    picked = all_samples[::stride][:args.n]
    print(f"[golden] {len(all_samples)} live_opp → 采样 {len(picked)}（stride={stride}）", flush=True)

    hands = []
    n_snaps = 0
    for s in picked:
        feat, streets, pidxs, priors, alive, snaps, _b3, _trunc = fz.run(s)
        res = model.residual(feat.astype(np.float32)) if model is not None else None
        rec = {"s": s, "y": int(s["y"]), "snaps": [int(x) for x in snaps], "streets": {}}
        if model is not None:
            rec["fdim"] = int(feat.shape[1])
            rec["feat"] = [float(x) for x in feat.reshape(-1)]   # row-major flat [T*fdim]
        for S in range(4):
            sp = int(snaps[S])
            if sp < 0:
                continue
            lp = np.asarray(priors[pidxs[sp]], dtype=np.float64)   # f32 因果 log m_prior
            av = alive[S]
            street_rec = {
                "snap": sp,
                "logprior": [float(x) for x in lp],
                "alive": [int(x) for x in av],
                "p": [float(x) for x in b2b_p(lp, av)],
            }
            if res is not None:
                street_rec["pred"] = [float(x) for x in pred_p(lp, res[sp], av, args.rscale)]
            rec["streets"][str(S)] = street_rec
            n_snaps += 1
        hands.append(rec)

    out = {
        "meta": {
            "n_hands": len(hands), "n_snaps": n_snaps,
            "strength_samples": 8000, "strength_seed": 20260702,
            "rscale": args.rscale, "has_pred": model is not None,
            "params": B2_PARAMS,
            "note": "Rust range.rs 因果 m_prior + mlrange 前向对拍 oracle；"
                    "比对 softmax-p / pred(rscale) max|Δ|<1e-4 + top-5",
        },
        "hands": hands,
    }
    os.makedirs(os.path.dirname(args.out), exist_ok=True)
    with open(args.out, "w") as f:
        json.dump(out, f)
    sz = os.path.getsize(args.out) / 1e6
    print(f"[golden] {len(hands)} 手 / {n_snaps} 街末快照 → {args.out} ({sz:.1f} MB)", flush=True)

    # 自检：抽第一手第一街末，打印 top-5 类，肉眼可核
    from range_ml_common import class_name
    for rec in hands[:1]:
        for S, st in rec["streets"].items():
            p = np.asarray(st["p"])
            top5 = np.lexsort((np.arange(169), -p))[:5]
            print(f"  self-check hand y={class_name(rec['y'])} street{S}: "
                  f"top5={[class_name(int(c)) for c in top5]}")
            break


if __name__ == "__main__":
    main()

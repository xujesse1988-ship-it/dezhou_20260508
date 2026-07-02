#!/usr/bin/env python3
"""M3 §6：导出 v1 GRU 权重为 Rust 手写前向可载入的格式（JSON，f64-repr）。

Rust `mlrange.rs` 前向逐义移植 `range_shadow.NpResidualGRU`（已对 torch 原版对拍
2e-6），载入本文件的 8 个权重张量即可复现。JSON（非二进制）沿用 strength169.json 先例
——低风险、serde_json 直读；f64 repr 精确表达 f32 权重值（Rust 侧 f64 前向）。

用法：.venv-rangeml/bin/python tools/range_export.py
  → artifacts/range_ml/range_v1_gru_weights.json
"""

import argparse
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--ckpt", default="artifacts/range_ml/range_v1_gru.pt")
    ap.add_argument("--out", default="artifacts/range_ml/range_v1_gru_weights.json")
    args = ap.parse_args()

    import torch

    ck = torch.load(args.ckpt, map_location="cpu", weights_only=False)
    assert not ck.get("use_shows"), "v1 前向不含亮牌记忆通道"
    sd = ck["model"]
    keys = ["inp.weight", "inp.bias", "gru.weight_ih_l0", "gru.weight_hh_l0",
            "gru.bias_ih_l0", "gru.bias_hh_l0", "head.weight", "head.bias"]
    weights = {}
    shapes = {}
    for k in keys:
        t = sd[k].detach().cpu().numpy()
        weights[k] = t.reshape(-1).astype(float).tolist()   # row-major flat, f64 repr
        shapes[k] = list(t.shape)
    out = {
        "meta": {
            "fdim": int(ck["fdim"]), "d": int(ck["d"]), "n_class": 169,
            "epoch": ck.get("epoch"), "val_nll": ck.get("val_nll"),
            "shapes": shapes,
            "note": "row-major flat f64; GRU gate order [r,z,n]（PyTorch 约定）",
        },
        "weights": weights,
    }
    os.makedirs(os.path.dirname(args.out), exist_ok=True)
    with open(args.out, "w") as f:
        json.dump(out, f)
    sz = os.path.getsize(args.out) / 1e6
    print(f"[export] fdim={ck['fdim']} d={ck['d']} → {args.out} ({sz:.1f} MB)", flush=True)
    for k in keys:
        print(f"  {k:22s} {shapes[k]}")


if __name__ == "__main__":
    main()
